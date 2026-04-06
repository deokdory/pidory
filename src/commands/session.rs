use poise::serenity_prelude as serenity;
use std::time::Duration;

use crate::{Context, Error};
use crate::db::repository;
use crate::i18n::Lang;

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

#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_CHANNELS",
    required_permissions = "MANAGE_CHANNELS"
)]
pub async fn del(
    ctx: Context<'_>,
    #[description = "Thread ID (defaults to current thread)"] thread_id: Option<String>,
) -> Result<(), Error> {
    let lang = ctx.data().config.language;
    let tid = match thread_id {
        Some(id) => id,
        None => {
            // Must be inside a thread
            match ctx.channel_id().to_channel(ctx.http()).await? {
                serenity::Channel::Guild(ch) if ch.thread_metadata.is_some() => {
                    ctx.channel_id().to_string()
                }
                _ => {
                    let reply = poise::CreateReply::default()
                        .content(format!("❌ {}", lang.not_in_thread()))
                        .ephemeral(true);
                    ctx.send(reply).await?;
                    return Ok(());
                }
            }
        }
    };

    let session = repository::get_session_by_thread(&ctx.data().db, &tid).await?;
    if session.is_none() {
        let reply = poise::CreateReply::default()
            .content(format!("❌ {}", lang.no_session_found(&tid)))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    // Kill session if running (ignore failure — process may have already exited)
    let _ = ctx.data().sessions.kill_session(&tid).await;
    ctx.data().session_skills.lock().await.remove(&tid);
    ctx.data().pending_permissions.lock().await.retain(|_, p| p.thread_id != tid);
    ctx.data().needs_context.lock().await.remove(&tid);
    ctx.data().turn_initiators.lock().await.remove(&tid);
    ctx.data().turn_participants.lock().await.remove(&tid);

    repository::delete_session(&ctx.data().db, &tid).await?;

    let reply = poise::CreateReply::default()
        .content(format!("✅ {}", lang.session_deleted()))
        .ephemeral(true);
    ctx.send(reply).await?;

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
            lang.session_status_display(&s.thread_id, &s.status, session_id_line, &last_active_line)
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
