mod interaction;
mod event_processor;
mod helpers;
pub(crate) mod interaction_kind;

pub use event_processor::process_turn_events;
pub(crate) use helpers::format_cli_command;
pub(crate) use helpers::shorten_model_name;
pub(crate) use helpers::format_ctx_suffix;
pub(crate) use helpers::sanitize_sender_body;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use poise::serenity_prelude::{ChannelId, Context, FullEvent, GuildId, MessageId, MessageType, UserId};
use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::db::repository;
use crate::error::PidoryError;
use crate::handler::attachment_download;
use crate::handler::cleanup::cleanup_session_state;
use crate::handler::emoji;
use crate::handler::emoji::ReactionStatus;
use crate::subprocess::parser::StreamEvent;
use crate::subprocess::session_manager::{QueuedMessage, ReplyContext, SenderInfo};
use crate::Data;

pub async fn handle_event(
    ctx: &Context,
    event: &FullEvent,
    data: &Data,
) -> Result<(), PidoryError> {
    // Background task (rate limit monitor 등)에 fresh Context 전달.
    // Shard reconnect 후 stale ShardMessenger 문제 방지.
    let _ = data.ctx_watch.send(ctx.clone());

    match event {
        FullEvent::Message { new_message } => handle_message(ctx, new_message, data).await,
        FullEvent::InteractionCreate { interaction } => {
            interaction::handle_interaction(ctx, interaction, data).await
        }
        FullEvent::ThreadUpdate { new, .. } => {
            if new.thread_metadata.as_ref().is_some_and(|m| m.archived) {
                handle_thread_closed(ctx, data, &new.id.to_string()).await
            } else {
                Ok(())
            }
        }
        FullEvent::ThreadDelete { thread, .. } => {
            handle_thread_closed(ctx, data, &thread.id.to_string()).await
        }
        _ => Ok(()),
    }
}

async fn handle_thread_closed(ctx: &Context, data: &Data, thread_id: &str) -> Result<(), PidoryError> {
    if !data.sessions.session_exists(thread_id).await {
        return Ok(());
    }

    {
        let mut guard = data.session_states.lock().await;
        if let Some(s) = guard.get_mut(thread_id)
            && !s.turn_participants.is_empty()
        {
            s.archived = true;
        }
    }

    if let Err(e) = data.sessions.kill_session(thread_id).await {
        warn!("Failed to kill session for closed thread {}: {}", thread_id, e);
    }

    let db = &data.db;
    if let Err(e) = repository::update_session_status(db, thread_id, "archived").await {
        warn!("Failed to update session status for closed thread {}: {}", thread_id, e);
    }

    cleanup_session_state(data, thread_id, ctx).await;

    tracing::info!(thread_id = %thread_id, "Session killed due to thread archive/delete");

    Ok(())
}

async fn resolve_reply_context(
    _ctx: &Context,
    message: &poise::serenity_prelude::Message,
) -> Option<ReplyContext> {
    // Use Gateway-resolved referenced_message only (no HTTP fallback to preserve queue order)
    let referenced = message.referenced_message.as_ref()?;
    let content = referenced.content.trim();

    // Include fallback text when content is empty (attachment-only, embed-only, etc.)
    let original_content = if content.is_empty() {
        "(텍스트 없음 — 이미지/파일/임베드만 있는 메시지)".to_string()
    } else {
        content.to_string()
    };

    Some(ReplyContext {
        original_content,
        original_author_name: referenced.author.name.clone(),
    })
}

async fn handle_message(
    ctx: &Context,
    new_message: &poise::serenity_prelude::Message,
    data: &Data,
) -> Result<(), PidoryError> {
    // 시스템 메시지 무시 (스레드 이름 변경, 핀 등)
    if !matches!(new_message.kind, MessageType::Regular | MessageType::InlineReply | MessageType::ThreadStarterMessage) {
        return Ok(());
    }

    // bot 자신의 메시지 무시
    if new_message.author.bot {
        return Ok(());
    }

    // guild ID 검증
    if new_message.guild_id != Some(GuildId::new(data.config.discord.guild_id)) {
        return Ok(());
    }

    let lang = data.config.language;

    // 스레드인지 확인
    let channel = match new_message.channel_id.to_channel(ctx).await {
        Ok(ch) => ch,
        Err(e) => {
            warn!("Failed to fetch channel {}: {}", new_message.channel_id, e);
            return Ok(());
        }
    };

    let guild_channel = match channel.guild() {
        Some(gc) => gc,
        None => return Ok(()),
    };

    if guild_channel.thread_metadata.is_none() {
        return Ok(());
    }

    // parent channel ID 추출
    let parent_channel_id = match guild_channel.parent_id {
        Some(pid) => pid.to_string(),
        None => return Ok(()),
    };

    // parent channel에 등록된 프로젝트 확인
    let db = &data.db;
    let project = match repository::get_project_by_channel(db, &parent_channel_id).await? {
        Some(p) => p,
        None => return Ok(()),
    };

    let thread_id = new_message.channel_id.to_string();
    tracing::info!(thread_id = %thread_id, "Message received in thread");
    let channel_id = new_message.channel_id;
    let msg_id = new_message.id;

    // reply context resolve (InlineReply인 경우)
    let reply_context = if new_message.kind == MessageType::InlineReply {
        resolve_reply_context(ctx, new_message).await
    } else {
        None
    };

    // 세션 DB 확인/생성
    let is_new_session;
    let session = match repository::get_session_by_thread(db, &thread_id).await? {
        Some(s) => {
            is_new_session = false;
            s
        }
        None => {
            tracing::info!("Creating new session for thread {}", thread_id);
            is_new_session = true;
            repository::create_session(db, &thread_id, &parent_channel_id).await?
        }
    };

    // disallowed_tools 결정
    let disallowed_tools: Vec<String> = match &project.disallowed_tools {
        Some(json_str) => serde_json::from_str(json_str).unwrap_or_else(|e| {
            warn!("Failed to parse disallowed_tools JSON: {}", e);
            data.config.claude.default_disallowed_tools.clone()
        }),
        None => data.config.claude.default_disallowed_tools.clone(),
    };

    // per-thread dispatch 직렬화 lock 획득.
    // get_or_create + try_acquire_session + (primary turn 시 restart) + send_message 전체를
    // 같은 lock 안에서 직렬화한다.
    // AllowAlways 후 두 메시지가 동시 도착해도 순서 보장 (#258, #298):
    //   M_A: lock 획득 → get_or_create → try_acquire=true → restart consume → respawn → send
    //   M_B: lock 대기 → get_or_create(새 inner 재사용) → try_acquire=false → restart skip (set 보존)
    let _dispatch_lock_arc = data.dispatch_locks.get_or_create(&thread_id).await;
    let _dispatch_guard = _dispatch_lock_arc.lock().await;

    // SessionManager: 세션 생성 또는 기존 재사용 (restart 없이 먼저 확보)
    match data
        .sessions
        .get_or_create(
            &thread_id,
            &project.path,
            session.session_id.as_deref(),
            &disallowed_tools,
            session.model.as_deref().or(data.config.claude.default_model.as_deref()),
            ctx.clone(),
            channel_id,
            data.db.clone(),
            lang,
            data.pending_permissions.clone(),
            data.pending_question_groups.clone(),
            data.config.discord.owner_id,
            crate::subprocess::supervisor::SessionCleanupHandles::from_data(data),
            data.config.discord.notification_channel_id.map(poise::serenity_prelude::ChannelId::new),
        )
        .await
    {
        Ok(result) => {
            tracing::info!(
                thread_id = %thread_id,
                evicted = result.evicted_thread_id.as_deref(),
                "Session get_or_create completed"
            );
            if let Some(evicted_tid) = result.evicted_thread_id {
                cleanup_session_state(data, &evicted_tid, ctx).await;
                if let Err(e) = repository::update_session_status(db, &evicted_tid, "idle").await {
                    tracing::warn!("Failed to update session status for evicted thread {}: {}", evicted_tid, e);
                }
            }
        }
        Err(e) => {
            error!("Failed to get_or_create session for thread {}: {}", thread_id, e);
            channel_id
                .say(ctx, format!("❌ {}", lang.session_create_failed(&e)))
                .await
                .map_err(|e| PidoryError::Discord(Box::new(e)))?;
            return Ok(());
        }
    }

    let compact_args = helpers::parse_compact_command(&new_message.content);
    let is_cli_command = compact_args.is_some();

    // 원자적 acquire: running이 아닌 경우에만 running으로 전환
    let acquired = repository::try_acquire_session(db, &thread_id).await?;

    if !acquired {
        // mid-turn inject: event_tx 없이 전송 (context inject 안 함, needs_context 소비 안 함)
        // pending_session_restart 의 thread_id 는 그대로 보존 — 다음 primary turn 시 재시도
        let mid_turn_downloaded_files =
            download_message_attachments(
                &new_message.attachments,
                &project.path,
                channel_id,
                msg_id,
                ctx,
                &data.config.attachment,
            ).await;

        let content = if let Some(args) = compact_args {
            helpers::format_cli_command("compact", args)
        } else {
            new_message.content.clone()
        };

        let sender_info = if compact_args.is_some() {
            None
        } else {
            let nick = new_message.member.as_ref().and_then(|m| m.nick.as_deref());
            let global = new_message.author.global_name.as_deref();
            let username = new_message.author.name.as_str();
            Some(SenderInfo {
                label: helpers::format_sender_label(nick, global, username),
            })
        };

        let msg = QueuedMessage {
            content,
            channel_id,
            message_id: msg_id,
            event_tx: None,
            triggered_by: new_message.author.id,
            cancelled: Arc::new(AtomicBool::new(false)),
            downloaded_files: mid_turn_downloaded_files.clone(),
            reply_context: reply_context.clone(),
            sender_info,
        };

        match data.sessions.send_message(&thread_id, msg).await {
            Ok(()) => {
                // CLI 커맨드가 성공적으로 큐잉된 후에만 flag 세팅
                if is_cli_command {
                    data.session_states.lock().await.entry(thread_id.clone()).or_default().needs_context = true;
                }
                // mid-turn inject 사용자를 participants에 추가
                data.session_states
                    .lock()
                    .await
                    .entry(thread_id.clone())
                    .or_default()
                    .turn_participants
                    .insert(new_message.author.id);
                let _ = channel_id
                    .create_reaction(
                        ctx,
                        msg_id,
                        poise::serenity_prelude::ReactionType::Unicode("📨".to_string()),
                    )
                    .await;
            }
            Err(e) if e.to_string().contains("queue full") => {
                for path in &mid_turn_downloaded_files {
                    let _ = tokio::fs::remove_file(path).await;
                }
                channel_id
                    .say(ctx, format!("❌ {}", lang.queue_full()))
                    .await
                    .map_err(|e| PidoryError::Discord(Box::new(e)))?;
                return Ok(());
            }
            Err(e) => {
                for path in &mid_turn_downloaded_files {
                    let _ = tokio::fs::remove_file(path).await;
                }
                channel_id
                    .say(ctx, format!("❌ {}", lang.error_with(&e)))
                    .await
                    .map_err(|e| PidoryError::Discord(Box::new(e)))?;
                return Ok(());
            }
        }

        return Ok(());
    }

    // AllowAlways 성공 후 subprocess restart 예약 처리 — primary turn 시작 시점에만 발동.
    // dispatch_lock 안에서 실행하여 동시 도착 메시지와의 race 방지 (#298).
    // mid-turn (acquired=false) 에서는 skip + set 에 thread_id 보존 → 다음 primary turn 시 재시도.
    // restart 후 get_or_create 재호출: SessionInner 제거 → 새 subprocess spawn (--resume).
    if data
        .pending_session_restart
        .lock()
        .await
        .remove(&thread_id)
    {
        if let Some(sid) = session.session_id.as_deref() {
            if let Err(e) = data
                .sessions
                .restart_for_settings_reload(&thread_id, sid)
                .await
            {
                tracing::warn!(
                    thread_id = %thread_id,
                    error = %e,
                    "restart_for_settings_reload failed (session may not exist yet); continuing"
                );
            }
            // SessionInner 가 제거됐으므로 같은 dispatch_lock 안에서 즉시 재spawn.
            match data
                .sessions
                .get_or_create(
                    &thread_id,
                    &project.path,
                    session.session_id.as_deref(),
                    &disallowed_tools,
                    session.model.as_deref().or(data.config.claude.default_model.as_deref()),
                    ctx.clone(),
                    channel_id,
                    data.db.clone(),
                    lang,
                    data.pending_permissions.clone(),
                    data.pending_question_groups.clone(),
                    data.config.discord.owner_id,
                    crate::subprocess::supervisor::SessionCleanupHandles::from_data(data),
                    data.config.discord.notification_channel_id.map(poise::serenity_prelude::ChannelId::new),
                )
                .await
            {
                Ok(result) => {
                    tracing::info!(
                        thread_id = %thread_id,
                        evicted = result.evicted_thread_id.as_deref(),
                        "Session respawned after settings reload restart"
                    );
                    if let Some(evicted_tid) = result.evicted_thread_id {
                        cleanup_session_state(data, &evicted_tid, ctx).await;
                        if let Err(e) = repository::update_session_status(db, &evicted_tid, "idle").await {
                            tracing::warn!("Failed to update session status for evicted thread {}: {}", evicted_tid, e);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to respawn session after restart for thread {}: {}", thread_id, e);
                    channel_id
                        .say(ctx, format!("❌ {}", lang.session_create_failed(&e)))
                        .await
                        .map_err(|e| PidoryError::Discord(Box::new(e)))?;
                    return Ok(());
                }
            }
        } else {
            tracing::warn!(
                thread_id = %thread_id,
                "pending_session_restart set but session_id is None; skipping restart"
            );
        }
    }

    // 직접 실행 경로: context inject 판정 (primary 경로만)
    let content = if let Some(args) = compact_args {
        helpers::format_cli_command("compact", args)
    } else {
        let had_needs_context = data.session_states.lock().await
            .get_mut(&thread_id)
            .map(|s| std::mem::replace(&mut s.needs_context, false))
            .unwrap_or(false);
        helpers::build_context_content(&new_message.content, is_new_session, had_needs_context, &guild_channel.name, lang)
    };

    // turn 시작: archived tombstone 클리어 (#314) + turn-scoped 필드 초기화 + turn_initiator 기록
    {
        let mut guard = data.session_states.lock().await;
        let s = guard.entry(thread_id.clone()).or_default();
        s.begin_turn(new_message.author.id);
        s.turn_initiator = Some(new_message.author.id);
    }

    // 첨부파일 있으면 ⏬ reaction 먼저
    if !new_message.attachments.is_empty() {
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Downloading)
            .await
            .ok();
    }

    let primary_downloaded_files =
        download_message_attachments(
            &new_message.attachments,
            &project.path,
            channel_id,
            msg_id,
            ctx,
            &data.config.attachment,
        ).await;

    emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Running)
        .await
        .ok();

    let sender_info = if compact_args.is_some() {
        None
    } else {
        let nick = new_message.member.as_ref().and_then(|m| m.nick.as_deref());
        let global = new_message.author.global_name.as_deref();
        let username = new_message.author.name.as_str();
        Some(SenderInfo {
            label: helpers::format_sender_label(nick, global, username),
        })
    };

    let (event_tx, event_rx) = mpsc::channel::<StreamEvent>(64);
    let msg = QueuedMessage {
        content: content.clone(),
        channel_id,
        message_id: msg_id,
        event_tx: Some(event_tx),
        triggered_by: new_message.author.id,
        cancelled: Arc::new(AtomicBool::new(false)),
        downloaded_files: primary_downloaded_files.clone(),
        reply_context: reply_context.clone(),
        sender_info,
    };

    if let Err(e) = data.sessions.send_message(&thread_id, msg).await {
        error!("Failed to send message to session {}: {}", thread_id, e);
        for path in &primary_downloaded_files {
            let _ = tokio::fs::remove_file(path).await;
        }
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
        repository::update_session_status(db, &thread_id, "error").await?;
        channel_id
            .say(ctx, format!("❌ {}", lang.message_send_failed(&e)))
            .await
            .map_err(|e| PidoryError::Discord(Box::new(e)))?;
        return Ok(());
    }

    // CLI 커맨드가 성공적으로 전송된 후에만 flag 세팅
    if is_cli_command {
        data.session_states.lock().await.entry(thread_id.clone()).or_default().needs_context = true;
    }

    // send_message 완료 후 dispatch lock 해제.
    // process_turn_events는 턴 완료까지 await하므로 반드시 lock 밖에서 실행.
    drop(_dispatch_guard);

    process_turn_events(
        ctx,
        event_rx,
        channel_id,
        msg_id,
        &thread_id,
        db,
        data.config.response.max_chunk_length,
        data.config.response.max_chunks,
        lang,
        data.config.discord.owner_id,
        data.config.footer.show_context_percent,
        data.session_states.clone(),
    )
    .await;

    Ok(())
}

pub async fn execute_in_session(
    ctx: &Context,
    data: &Data,
    thread_id: &str,
    channel_id: ChannelId,
    msg_id: MessageId,
    content: &str,
    triggered_by: UserId,
) -> Result<(), PidoryError> {
    let db = &data.db;

    // per-thread dispatch 직렬화 lock 획득 (try_acquire_session 이전) (#258)
    let _dispatch_lock_arc = data.dispatch_locks.get_or_create(thread_id).await;
    let _dispatch_guard = _dispatch_lock_arc.lock().await;

    let compact_args = helpers::parse_compact_command(content);
    let is_cli_command = compact_args.is_some();

    let acquired = repository::try_acquire_session(db, thread_id).await?;

    if !acquired {
        // mid-turn inject: event_tx 없이 전송
        let effective_content = if let Some(args) = compact_args {
            helpers::format_cli_command("compact", args)
        } else {
            content.to_string()
        };
        let msg = QueuedMessage {
            content: effective_content,
            channel_id,
            message_id: msg_id,
            event_tx: None,
            triggered_by,
            cancelled: Arc::new(AtomicBool::new(false)),
            downloaded_files: Vec::new(),
            reply_context: None,
            sender_info: None,
        };
        data.sessions.send_message(thread_id, msg).await?;
        if is_cli_command {
            data.session_states.lock().await.entry(thread_id.to_string()).or_default().needs_context = true;
        }
        // mid-turn inject 사용자를 participants에 추가
        data.session_states
            .lock()
            .await
            .entry(thread_id.to_string())
            .or_default()
            .turn_participants
            .insert(triggered_by);
        return Ok(());
    }

    // 직접 실행
    // stale needs_context 정리 (CLI 커맨드가 아닌 경우에만 — CLI 커맨드는 send 후 insert)
    if !is_cli_command
        && let Some(s) = data.session_states.lock().await.get_mut(thread_id)
    {
        s.needs_context = false;
    }

    let effective_content = if let Some(args) = compact_args {
        helpers::format_cli_command("compact", args)
    } else {
        content.to_string()
    };

    emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Running)
        .await
        .ok();

    let (event_tx, event_rx) = mpsc::channel::<StreamEvent>(64);
    let msg = QueuedMessage {
        content: effective_content,
        channel_id,
        message_id: msg_id,
        event_tx: Some(event_tx),
        triggered_by,
        cancelled: Arc::new(AtomicBool::new(false)),
        downloaded_files: Vec::new(),
        reply_context: None,
        sender_info: None,
    };

    // archived tombstone 클리어 (#314) + turn-scoped 필드 초기화 (skill 직접 실행 경로).
    // turn_initiator는 skill 경로 정책상 설정하지 않는다.
    {
        let mut guard = data.session_states.lock().await;
        let s = guard.entry(thread_id.to_string()).or_default();
        s.begin_turn(triggered_by);
    }

    if let Err(e) = data.sessions.send_message(thread_id, msg).await {
        error!("Failed to send message to session {}: {}", thread_id, e);
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
        repository::update_session_status(db, thread_id, "error").await?;
        return Err(e);
    }

    if is_cli_command {
        data.session_states.lock().await.entry(thread_id.to_string()).or_default().needs_context = true;
    }

    // send_message 완료 후 dispatch lock 해제.
    // process_turn_events는 턴 완료까지 await하므로 반드시 lock 밖에서 실행.
    drop(_dispatch_guard);

    let thread_id_string = thread_id.to_string();

    process_turn_events(
        ctx,
        event_rx,
        channel_id,
        msg_id,
        thread_id,
        db,
        data.config.response.max_chunk_length,
        data.config.response.max_chunks,
        data.config.language,
        data.config.discord.owner_id,
        data.config.footer.show_context_percent,
        data.session_states.clone(),
    )
    .await;

    if is_cli_command {
        // cli 명령 종료 시 tracker 폐기 (Present일 때만 take, CheckedOut이면 그쪽이 cleanup 책임)
        let tracker = {
            let mut guard = data.session_states.lock().await;
            guard.get_mut(&thread_id_string).and_then(|s| s.take_present_todo_tracker())
        };
        if let Some(mut tracker) = tracker {
            tracker.cleanup(ctx).await;
        }
    }

    Ok(())
}

async fn download_message_attachments(
    attachments: &[poise::serenity_prelude::Attachment],
    project_path: &str,
    channel_id: ChannelId,
    msg_id: MessageId,
    ctx: &Context,
    attachment_config: &crate::config::AttachmentConfig,
) -> Vec<String> {
    if attachments.is_empty() {
        return Vec::new();
    }
    let (paths, errors) = attachment_download::download_attachments(
        attachments,
        std::path::Path::new(project_path),
        channel_id.get(),
        msg_id.get(),
        attachment_config.max_file_size_bytes(),
        attachment_config.max_aggregate_size_bytes(),
        attachment_config.download_timeout_secs,
    )
    .await;
    for err in &errors {
        let _ = channel_id.say(ctx, format!("⚠️ {}", err)).await;
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::helpers::{build_context_content, format_cli_command, format_ctx_suffix};
    use crate::i18n::Lang;

    #[test]
    fn inject_on_new_session() {
        let result = build_context_content("안녕", true, false, "테스트 스레드", Lang::Ko);
        assert!(result.contains("<system-reminder>"));
        assert!(result.contains("테스트 스레드"));
        assert!(result.ends_with("안녕"));
    }

    #[test]
    fn inject_after_new_command() {
        let result = build_context_content("작업 시작", false, true, "스레드", Lang::Ko);
        assert!(result.contains("<system-reminder>"));
    }

    #[test]
    fn no_inject_normal_message() {
        let result = build_context_content("일반 메시지", false, false, "스레드", Lang::Ko);
        assert!(!result.contains("<system-reminder>"));
        assert_eq!(result, "일반 메시지");
    }

    #[test]
    fn test_format_ctx_suffix() {
        assert_eq!(format_ctx_suffix(26150, 1000000, true), " · ctx:2%");
        assert_eq!(format_ctx_suffix(420000, 1000000, true), " · ctx:42%");
        assert_eq!(format_ctx_suffix(0, 0, true), "");
        assert_eq!(format_ctx_suffix(100, 0, true), "");
        assert_eq!(format_ctx_suffix(1000000, 1000000, true), " · ctx:100%");
        assert_eq!(format_ctx_suffix(26150, 1000000, false), "");
        assert_eq!(format_ctx_suffix(420000, 1000000, false), "");
    }

    #[test]
    fn format_cli_command_name_only() {
        assert_eq!(
            format_cli_command("clear", None),
            "<command-name>/clear</command-name>"
        );
    }

    #[test]
    fn format_cli_command_with_args() {
        assert_eq!(
            format_cli_command("skill", Some("commit")),
            "<command-name>/skill</command-name><command-message>commit</command-message>"
        );
    }

    #[test]
    fn format_cli_command_strips_leading_slash() {
        assert_eq!(
            format_cli_command("/clear", None),
            "<command-name>/clear</command-name>"
        );
    }

    #[test]
    fn format_cli_command_empty_args_ignored() {
        assert_eq!(
            format_cli_command("compact", Some("")),
            "<command-name>/compact</command-name>"
        );
    }

    #[test]
    fn format_cli_command_escapes_xml() {
        assert_eq!(
            format_cli_command("skill", Some("echo </command-message>")),
            "<command-name>/skill</command-name><command-message>echo &lt;/command-message&gt;</command-message>"
        );
    }

}
