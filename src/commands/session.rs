use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use poise::serenity_prelude as serenity;
use tokio::sync::mpsc;

use crate::{Context, Error};
use crate::db::repository;
use crate::handler::cleanup::cleanup_session_state;
use crate::handler::message::process_turn_events;
use crate::i18n::Lang;
use crate::subprocess::parser::StreamEvent;
use crate::subprocess::session_manager::QueuedMessage;

/// Parse a naive datetime string "YYYY-MM-DD HH:MM:SS[.f]" into Unix seconds.
/// Returns None if parsing fails.
fn parse_datetime_secs(dt_str: &str) -> Option<i64> {
    // Expected format from SQLite: "2024-01-01 12:00:00" or "2024-01-01 12:00:00.123"
    let s = dt_str.trim();
    let (date_part, time_part) = if let Some(idx) = s.find(' ') {
        (&s[..idx], &s[idx + 1..])
    } else {
        return None;
    };

    let mut date_parts = date_part.splitn(3, '-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;

    let time_no_frac = time_part.split('.').next()?;
    let mut time_parts = time_no_frac.splitn(3, ':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let min: i64 = time_parts.next()?.parse().ok()?;
    let sec: i64 = time_parts.next()?.parse().ok()?;

    // Compute days since Unix epoch using a simple formula (ignores leap seconds).
    // Days from 1970-01-01 to year-month-day using Julian Day Number difference.
    let a = (14 - month) / 12;
    let y = year + 4800 - a;
    let m = month + 12 * a - 3;
    let jdn = day + (153 * m + 2) / 5 + 365 * y + y / 4 - y / 100 + y / 400 - 32045;
    // Julian Day Number of 1970-01-01 is 2440588
    let days_since_epoch = jdn - 2440588;
    Some(days_since_epoch * 86400 + hour * 3600 + min * 60 + sec)
}

/// Format an ISO-8601 datetime string as relative time.
/// Falls back to the raw string if parsing fails.
fn format_relative(dt_str: &str, lang: Lang) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let then_secs = match parse_datetime_secs(dt_str) {
        Some(s) => s,
        None => return dt_str.to_string(),
    };

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let diff = (now_secs - then_secs).max(0) as u64;
    lang.format_relative_time(diff)
}

/// 전역 세션 현황 조회
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_CHANNELS",
    required_permissions = "MANAGE_CHANNELS"
)]
pub async fn sessions(ctx: Context<'_>) -> Result<(), Error> {
    let data = ctx.data();
    let lang = data.config.language;
    let infos = data.sessions.get_session_info().await;
    let max_sessions = data.config.claude.max_sessions;

    if infos.is_empty() {
        ctx.send(
            poise::CreateReply::default()
                .ephemeral(true)
                .content(format!("{}\n{}", lang.active_sessions_header(0, max_sessions), lang.no_active_sessions_short())),
        )
        .await?;
        return Ok(());
    }

    let idle_timeout = Duration::from_secs(data.config.claude.idle_timeout_secs);
    let mut lines = vec![lang.active_sessions_header(infos.len(), max_sessions)];

    for info in &infos {
        let status = if info.is_turn_active {
            lang.running_status().to_string()
        } else {
            lang.format_idle(info.idle_duration.as_secs())
        };
        let bg = if info.has_bg_tasks { lang.bg_tasks_suffix() } else { "" };
        let warn = if !info.is_turn_active
            && idle_timeout.as_secs() > 0
            && info.idle_duration > idle_timeout * 80 / 100
        {
            " ⚠️"
        } else {
            ""
        };
        lines.push(format!("• <#{}> — {}{}{}", info.thread_id, status, bg, warn));
    }

    ctx.send(
        poise::CreateReply::default()
            .ephemeral(true)
            .content(lines.join("\n")),
    )
    .await?;
    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_CHANNELS",
    required_permissions = "MANAGE_CHANNELS"
)]
pub async fn list(
    ctx: Context<'_>,
    #[description = "Channel (defaults to current channel)"] channel: Option<serenity::ChannelId>,
) -> Result<(), Error> {
    let channel_id = channel
        .unwrap_or_else(|| ctx.channel_id())
        .to_string();
    let lang = ctx.data().config.language;

    let sessions =
        repository::list_sessions_by_channel(&ctx.data().db, &channel_id).await?;

    let content = if sessions.is_empty() {
        lang.no_active_sessions_short().to_string()
    } else {
        let mut lines = vec![lang.active_sessions_list_header().to_string()];
        for s in &sessions {
            let thread_mention = format!("<#{}>", s.thread_id);
            let since = s
                .last_active_at
                .as_deref()
                .map(|t| lang.session_list_since(&format_relative(t, lang)))
                .unwrap_or_default();
            let session_short = s
                .session_id
                .as_deref()
                .map(|id| lang.session_list_id(&id[..id.len().min(8)]))
                .unwrap_or_default();
            lines.push(lang.session_list_row(&thread_mention, &s.status, &session_short, &since));
        }
        lines.join("\n")
    };

    let reply = poise::CreateReply::default()
        .content(content)
        .ephemeral(true);
    ctx.send(reply).await?;

    Ok(())
}

/// 세션을 슬립 상태로 전환 (다음 메시지에서 자동 재개)
#[poise::command(slash_command, guild_only)]
pub async fn sleep(ctx: Context<'_>) -> Result<(), Error> {
    let data = ctx.data();
    let thread_id = ctx.channel_id().to_string();
    let lang = data.config.language;

    // 1. DB 세션 존재 확인
    let session = repository::get_session_by_thread(&data.db, &thread_id).await?;
    if session.is_none() {
        ctx.say(format!("❌ {}", lang.no_session_in_thread())).await?;
        return Ok(());
    }

    // 2. 권한 체크: 세션 시작자 또는 owner만 허용
    let triggered_by = data.turn_initiators.lock().await.get(&thread_id).copied();
    let is_owner = ctx.author().id == serenity::UserId::new(data.config.discord.owner_id);

    let allowed = match triggered_by {
        Some(tb) => ctx.author().id == tb || is_owner,
        None => is_owner,
    };

    if !allowed {
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.no_permission()))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    // 3. 턴 활성 체크
    let is_active = data
        .sessions
        .get_session_info()
        .await
        .into_iter()
        .find(|info| info.thread_id == thread_id)
        .map(|info| info.is_turn_active)
        .unwrap_or(false);

    if is_active {
        ctx.say(format!("❌ {}", lang.sleep_turn_active())).await?;
        return Ok(());
    }

    // 3. kill_session best-effort
    if let Err(e) = data.sessions.kill_session(&thread_id).await {
        tracing::debug!(thread_id = %thread_id, error = %e, "sleep: kill_session failed (best-effort)");
    }

    // 4. DB status → "idle"
    repository::update_session_status(&data.db, &thread_id, "idle").await?;

    // 5. in-memory cleanup
    cleanup_session_state(data, &thread_id, ctx.serenity_context()).await;

    // 6. 응답
    ctx.say(format!("-# 😴 {}", lang.session_slept())).await?;
    Ok(())
}

/// 진행 중인 Claude Code 작업 중단
#[poise::command(slash_command, guild_only)]
pub async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    let channel_id = ctx.channel_id();
    let thread_id = channel_id.to_string();
    let data = ctx.data();
    let lang = data.config.language;

    if !data.sessions.session_exists(&thread_id).await {
        ctx.say(format!("❌ {}", lang.no_session_in_thread())).await?;
        return Ok(());
    }

    // triggered_by 체크: 세션을 시작한 사람만 중단 가능 (owner 는 fallback)
    let triggered_by = data.turn_initiators.lock().await.get(&thread_id).copied();
    let is_owner = ctx.author().id == serenity::UserId::new(data.config.discord.owner_id);

    let allowed = match triggered_by {
        Some(tb) => ctx.author().id == tb || is_owner,
        None => is_owner,
    };

    if !allowed {
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.no_permission()))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    match data.sessions.interrupt_session(&thread_id).await {
        Ok(()) => {
            ctx.say(format!("-# ⛔ {}", lang.interrupted())).await?;
        }
        Err(e) => {
            ctx.say(format!("❌ {}", lang.interrupt_failed(&e))).await?;
        }
    }

    Ok(())
}

/// 현재 턴을 인터럽트하고 새 턴을 자동 시작
#[poise::command(slash_command, guild_only)]
pub async fn kick(
    ctx: Context<'_>,
    #[description = "재시작 시 전달할 메시지"] message: Option<String>,
) -> Result<(), Error> {
    let channel_id = ctx.channel_id();
    let thread_id = channel_id.to_string();
    let data = ctx.data();
    let lang = data.config.language;

    // 1. 세션 존재 체크
    if !data.sessions.session_exists(&thread_id).await {
        ctx.say(format!("❌ {}", lang.no_session_in_thread())).await?;
        return Ok(());
    }

    // 2. cooldown 체크 (5초)
    {
        let cooldowns = data.kick_cooldowns.lock().await;
        if let Some(last_kick) = cooldowns.get(&thread_id)
            && last_kick.elapsed() < Duration::from_secs(5)
        {
            ctx.send(
                poise::CreateReply::default()
                    .content(format!("❌ {}", lang.kick_cooldown()))
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
    }

    // 3. 활성 턴 체크
    let is_active = data
        .sessions
        .get_session_info()
        .await
        .into_iter()
        .find(|info| info.thread_id == thread_id)
        .map(|info| info.is_turn_active)
        .unwrap_or(false);

    if !is_active {
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.kick_no_active_turn()))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    // 4. 권한 체크
    let triggered_by = data.turn_initiators.lock().await.get(&thread_id).copied();
    let is_owner = ctx.author().id == serenity::UserId::new(data.config.discord.owner_id);

    let allowed = match triggered_by {
        Some(tb) => ctx.author().id == tb || is_owner,
        None => is_owner,
    };

    if !allowed {
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.no_permission()))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    // 5. last_tool_name 조회
    let last_tool = data
        .last_tool_name
        .lock()
        .await
        .get(&thread_id)
        .cloned()
        .unwrap_or_else(|| lang.none_placeholder().to_string());

    // 6. kick_pending 등록 (process_turn_events에서 자연 완료 시 제거됨)
    data.kick_pending.lock().await.insert(thread_id.clone());

    // 7. interrupt_session() 호출
    if let Err(e) = data.sessions.interrupt_session(&thread_id).await {
        data.kick_pending.lock().await.remove(&thread_id);
        ctx.say(format!("❌ {}", lang.interrupt_failed(&e))).await?;
        return Ok(());
    }

    // cooldown 기록
    data.kick_cooldowns
        .lock()
        .await
        .insert(thread_id.clone(), std::time::Instant::now());

    // 8. Discord 확인 메시지 전송
    let reply = ctx
        .say(format!("-# ⛔ {}", lang.kicked()))
        .await?;
    let kick_msg = reply.into_message().await?;
    let kick_msg_id = kick_msg.id;

    // system-reminder 메시지 구성
    let content = {
        let reminder = lang.kick_system_reminder(&last_tool);
        match &message {
            Some(msg) => format!("{}\n\n{}", reminder, msg),
            None => reminder,
        }
    };

    // 9. background task에서 DB status poll → 새 턴 시작
    let sessions = Arc::clone(&data.sessions);
    let db = data.db.clone();
    let config = Arc::clone(&data.config);
    let session_skills = Arc::clone(&data.session_skills);
    let turn_participants = Arc::clone(&data.turn_participants);
    let last_tool_name = Arc::clone(&data.last_tool_name);
    let archived_threads = Arc::clone(&data.archived_threads);
    let kick_pending = Arc::clone(&data.kick_pending);
    let turn_initiators = Arc::clone(&data.turn_initiators);
    let needs_context = Arc::clone(&data.needs_context);
    let todo_trackers = Arc::clone(&data.todo_trackers);
    let next_step_buttons = Arc::clone(&data.next_step_buttons);
    let author_id = ctx.author().id;
    let mut ctx_rx = data.ctx_watch.subscribe();

    tokio::spawn(async move {
        for _ in 0..25 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            match repository::get_session_by_thread(&db, &thread_id).await {
                Ok(Some(session)) if session.status == "idle" || session.status == "error" => {
                    // W1: kick_pending 확인 — 자연 완료 시 process_turn_events가 제거함
                    if !kick_pending.lock().await.remove(&thread_id) {
                        ctx_rx.mark_changed();
                        let serenity_ctx = ctx_rx.borrow_and_update().clone();
                        let _ = channel_id
                            .say(&serenity_ctx, format!("-# ℹ️ {}", lang.kick_natural_completion()))
                            .await;
                        return;
                    }

                    // W2: error 상태 처리
                    if session.status == "error" {
                        ctx_rx.mark_changed();
                        let serenity_ctx = ctx_rx.borrow_and_update().clone();
                        let _ = channel_id
                            .say(&serenity_ctx, format!("-# ⚠️ {}", lang.kick_error_state()))
                            .await;
                        return;
                    }

                    let acquired = match repository::try_acquire_session(&db, &thread_id).await {
                        Ok(a) => a,
                        Err(e) => {
                            tracing::warn!("kick: try_acquire_session failed: {}", e);
                            return;
                        }
                    };

                    // W3: 다른 메시지가 선점한 경우 알림
                    if !acquired {
                        ctx_rx.mark_changed();
                        let serenity_ctx = ctx_rx.borrow_and_update().clone();
                        let _ = channel_id
                            .say(&serenity_ctx, format!("-# ℹ️ {}", lang.kick_preempted()))
                            .await;
                        return;
                    }

                    ctx_rx.mark_changed();
                    let serenity_ctx = ctx_rx.borrow_and_update().clone();

                    needs_context.lock().await.remove(&thread_id);
                    // S1: stale tool name 방지
                    last_tool_name.lock().await.remove(&thread_id);

                    crate::handler::emoji::set_reaction(
                        &serenity_ctx,
                        channel_id,
                        kick_msg_id,
                        crate::handler::emoji::ReactionStatus::Running,
                    )
                    .await
                    .ok();

                    let (event_tx, event_rx) = mpsc::channel::<StreamEvent>(64);
                    let msg = QueuedMessage {
                        content: content.clone(),
                        channel_id,
                        message_id: kick_msg_id,
                        event_tx: Some(event_tx),
                        triggered_by: author_id,
                        cancelled: Arc::new(AtomicBool::new(false)),
                        downloaded_files: Vec::new(),
                        reply_context: None,
                    };

                    turn_participants
                        .lock()
                        .await
                        .insert(thread_id.clone(), std::collections::HashSet::from([author_id]));
                    turn_initiators.lock().await.insert(thread_id.clone(), author_id);

                    if let Err(e) = sessions.send_message(&thread_id, msg).await {
                        tracing::error!("kick: send_message failed: {}", e);
                        let _ = repository::update_session_status(&db, &thread_id, "error").await;
                        return;
                    }

                    let todo_tracker = {
                        let mut map = todo_trackers.lock().await;
                        map.entry(thread_id.clone())
                            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(
                                crate::handler::todo_tracker::TodoTracker::new(channel_id)
                            )))
                            .clone()
                    };

                    process_turn_events(
                        &serenity_ctx,
                        event_rx,
                        channel_id,
                        kick_msg_id,
                        &thread_id,
                        &db,
                        config.response.max_chunk_length,
                        config.response.max_chunks,
                        session_skills.clone(),
                        config.language,
                        config.discord.owner_id,
                        turn_participants.clone(),
                        archived_threads.clone(),
                        last_tool_name.clone(),
                        kick_pending.clone(),
                        todo_tracker.clone(),
                        next_step_buttons.clone(),
                    )
                    .await;

                    return;
                }
                Ok(None) => return,
                _ => continue,
            }
        }

        // 타임아웃 — kick_pending 정리
        kick_pending.lock().await.remove(&thread_id);
        ctx_rx.mark_changed();
        let serenity_ctx = ctx_rx.borrow_and_update().clone();
        let _ = channel_id
            .say(&serenity_ctx, format!("❌ {}", lang.kick_timeout()))
            .await;
    });

    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_CHANNELS",
    required_permissions = "MANAGE_CHANNELS"
)]
pub async fn status(
    ctx: Context<'_>,
    #[description = "Thread ID (defaults to current thread)"] thread_id: Option<String>,
) -> Result<(), Error> {
    let tid = match thread_id {
        Some(id) => id,
        None => ctx.channel_id().to_string(),
    };
    let lang = ctx.data().config.language;

    let session = repository::get_session_by_thread(&ctx.data().db, &tid).await?;

    let content = match session {
        None => format!("❌ {}", lang.no_session_found(&tid)),
        Some(s) => {
            let session_id_line = s
                .session_id
                .as_deref()
                .unwrap_or(lang.none_placeholder());
            let last_active_line = s
                .last_active_at
                .as_deref()
                .map(|t| format_relative(t, lang))
                .unwrap_or_else(|| lang.never_placeholder().to_string());
            let model_line = s.model.as_deref().unwrap_or("default");
            lang.session_status_display(&s.thread_id, &s.status, session_id_line, &last_active_line, model_line)
        }
    };

    let reply = poise::CreateReply::default()
        .content(content)
        .ephemeral(true);
    ctx.send(reply).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_idle_seconds() {
        assert_eq!(Lang::En.format_idle(30), "idle 30s");
        assert_eq!(Lang::Ko.format_idle(30), "유휴 30초");
    }

    #[test]
    fn format_idle_minutes() {
        assert_eq!(Lang::En.format_idle(150), "idle 2m");
        assert_eq!(Lang::Ko.format_idle(150), "유휴 2분");
    }

    #[test]
    fn format_idle_hours() {
        assert_eq!(Lang::En.format_idle(7200), "idle 2h0m");
        assert_eq!(Lang::Ko.format_idle(7200), "유휴 2시간0분");
    }

    #[test]
    fn format_idle_hours_minutes() {
        assert_eq!(Lang::En.format_idle(5430), "idle 1h30m");
        assert_eq!(Lang::Ko.format_idle(5430), "유휴 1시간30분");
    }
}
