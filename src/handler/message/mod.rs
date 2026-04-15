mod interaction;
mod event_processor;
mod helpers;

pub use event_processor::process_turn_events;
pub(crate) use helpers::format_cli_command;
pub(crate) use helpers::shorten_model_name;
pub(crate) use helpers::format_ctx_suffix;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use poise::serenity_prelude::{ChannelId, Context, FullEvent, GuildId, MessageId, MessageType, UserId};
use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::db::repository;
use crate::error::PidoryError;
use crate::handler::attachment_download;
use crate::handler::emoji;
use crate::handler::emoji::ReactionStatus;
use crate::subprocess::parser::StreamEvent;
use crate::subprocess::session_manager::{QueuedMessage, ReplyContext};
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

    if data.turn_participants.lock().await.contains_key(thread_id) {
        data.archived_threads.lock().await.insert(thread_id.to_string());
    }

    if let Err(e) = data.sessions.kill_session(thread_id).await {
        warn!("Failed to kill session for closed thread {}: {}", thread_id, e);
    }

    let db = &data.db;
    if let Err(e) = repository::update_session_status(db, thread_id, "archived").await {
        warn!("Failed to update session status for closed thread {}: {}", thread_id, e);
    }

    data.pending_permissions.lock().await.retain(|_, p| p.thread_id != thread_id);
    data.pending_question_groups.lock().await.retain(|_, g| g.thread_id != thread_id);
    data.pending_resets.lock().await.retain(|_, r| r.thread_id != thread_id);
    data.session_skills.lock().await.remove(thread_id);
    data.needs_context.lock().await.remove(thread_id);
    data.turn_initiators.lock().await.remove(thread_id);
    data.turn_participants.lock().await.remove(thread_id);

    if let Some(tracker) = data.todo_trackers.lock().await.remove(thread_id) {
        tracker.lock().await.cleanup(ctx).await;
    }

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

    // SessionManager: 세션 생성 또는 기존 재사용
    match data
        .sessions
        .get_or_create(
            &thread_id,
            &project.path,
            session.session_id.as_deref(),
            &disallowed_tools,
            ctx.clone(),
            channel_id,
            data.db.clone(),
            lang,
            data.pending_permissions.clone(),
            data.pending_question_groups.clone(),
            data.config.discord.owner_id,
            data.todo_trackers.clone(),
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
                data.pending_permissions.lock().await.retain(|_, p| p.thread_id != evicted_tid);
                data.pending_question_groups.lock().await.retain(|_, g| g.thread_id != evicted_tid);
                data.session_skills.lock().await.remove(&evicted_tid);
                data.needs_context.lock().await.remove(&evicted_tid);
                data.turn_initiators.lock().await.remove(&evicted_tid);
                data.turn_participants.lock().await.remove(&evicted_tid);
                data.last_tool_name.lock().await.remove(&evicted_tid);
                data.kick_cooldowns.lock().await.remove(&evicted_tid);
                data.kick_pending.lock().await.remove(&evicted_tid);
                if let Err(e) = repository::update_session_status(db, &evicted_tid, "idle").await {
                    tracing::warn!("Failed to update session status for evicted thread {}: {}", evicted_tid, e);
                }
                if let Ok(id) = evicted_tid.parse::<u64>() {
                    ChannelId::new(id)
                        .say(ctx, format!("-# ⚠️ {}", lang.session_evicted()))
                        .await
                        .ok();
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

    let is_reset_command = helpers::is_context_reset_command(&new_message.content);
    let compact_args = helpers::parse_compact_command(&new_message.content);
    let is_cli_command = is_reset_command || compact_args.is_some();

    // 원자적 acquire: running이 아닌 경우에만 running으로 전환
    let acquired = repository::try_acquire_session(db, &thread_id).await?;

    if !acquired {
        // mid-turn inject: event_tx 없이 전송 (context inject 안 함, needs_context 소비 안 함)
        let mid_turn_downloaded_files =
            download_message_attachments(
                &new_message.attachments,
                &project.path,
                channel_id,
                msg_id,
                ctx,
                &data.config.attachment,
            ).await;

        let content = if is_reset_command {
            helpers::format_cli_command("clear", None)
        } else if let Some(args) = compact_args {
            helpers::format_cli_command("compact", args)
        } else {
            new_message.content.clone()
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
        };

        match data.sessions.send_message(&thread_id, msg).await {
            Ok(()) => {
                // CLI 커맨드가 성공적으로 큐잉된 후에만 flag 세팅
                if is_cli_command {
                    data.needs_context.lock().await.insert(thread_id.clone());
                }
                // mid-turn inject 사용자를 participants에 추가
                data.turn_participants
                    .lock()
                    .await
                    .entry(thread_id.clone())
                    .or_default()
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

    // 직접 실행 경로: context inject 판정 (primary 경로만)
    let content = if is_reset_command {
        helpers::format_cli_command("clear", None)
    } else if let Some(args) = compact_args {
        helpers::format_cli_command("compact", args)
    } else {
        let had_needs_context = data.needs_context.lock().await.remove(&thread_id);
        helpers::build_context_content(&new_message.content, is_new_session, had_needs_context, &guild_channel.name, lang)
    };

    // turn 시작: 이 turn 의 triggering user 를 기록 (permission 위임용)
    data.turn_initiators
        .lock()
        .await
        .insert(thread_id.clone(), new_message.author.id);

    // turn_participants 초기화: 새 turn 시작 시 author 만 포함
    data.turn_participants
        .lock()
        .await
        .insert(thread_id.clone(), std::collections::HashSet::from([new_message.author.id]));

    data.last_tool_name.lock().await.remove(&thread_id);

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
        data.needs_context.lock().await.insert(thread_id.clone());
    }

    let todo_tracker = {
        let mut map = data.todo_trackers.lock().await;
        map.entry(thread_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(
                crate::handler::todo_tracker::TodoTracker::new(channel_id)
            )))
            .clone()
    };

    process_turn_events(
        ctx,
        event_rx,
        channel_id,
        msg_id,
        &thread_id,
        db,
        data.config.response.max_chunk_length,
        data.config.response.max_chunks,
        data.session_skills.clone(),
        lang,
        data.config.discord.owner_id,
        data.turn_participants.clone(),
        data.archived_threads.clone(),
        data.last_tool_name.clone(),
        data.kick_pending.clone(),
        todo_tracker.clone(),
    )
    .await;

    if is_reset_command {
        todo_tracker.lock().await.cleanup(ctx).await;
        data.todo_trackers.lock().await.remove(&thread_id);
    }

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

    let is_reset_command = helpers::is_context_reset_command(content);
    let compact_args = helpers::parse_compact_command(content);
    let is_cli_command = is_reset_command || compact_args.is_some();

    let effective_content = if is_reset_command {
        helpers::format_cli_command("clear", None)
    } else if let Some(args) = compact_args {
        helpers::format_cli_command("compact", args)
    } else {
        content.to_string()
    };

    let acquired = repository::try_acquire_session(db, thread_id).await?;

    if !acquired {
        // mid-turn inject: event_tx 없이 전송
        let msg = QueuedMessage {
            content: effective_content,
            channel_id,
            message_id: msg_id,
            event_tx: None,
            triggered_by,
            cancelled: Arc::new(AtomicBool::new(false)),
            downloaded_files: Vec::new(),
            reply_context: None,
        };
        data.sessions.send_message(thread_id, msg).await?;
        if is_cli_command {
            data.needs_context.lock().await.insert(thread_id.to_string());
        }
        // mid-turn inject 사용자를 participants에 추가
        data.turn_participants
            .lock()
            .await
            .entry(thread_id.to_string())
            .or_default()
            .insert(triggered_by);
        return Ok(());
    }

    // 직접 실행
    // stale needs_context 정리 (CLI 커맨드가 아닌 경우에만 — CLI 커맨드는 send 후 insert)
    if !is_cli_command {
        data.needs_context.lock().await.remove(thread_id);
    }

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
    };

    // turn_participants 초기화 (skill 직접 실행 경로)
    data.turn_participants
        .lock()
        .await
        .insert(thread_id.to_string(), std::collections::HashSet::from([triggered_by]));

    data.last_tool_name.lock().await.remove(thread_id);

    if let Err(e) = data.sessions.send_message(thread_id, msg).await {
        error!("Failed to send message to session {}: {}", thread_id, e);
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
        repository::update_session_status(db, thread_id, "error").await?;
        return Err(e);
    }

    if is_cli_command {
        data.needs_context.lock().await.insert(thread_id.to_string());
    }

    let thread_id_string = thread_id.to_string();
    let todo_tracker = {
        let mut map = data.todo_trackers.lock().await;
        map.entry(thread_id_string.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(
                crate::handler::todo_tracker::TodoTracker::new(channel_id)
            )))
            .clone()
    };

    process_turn_events(
        ctx,
        event_rx,
        channel_id,
        msg_id,
        thread_id,
        db,
        data.config.response.max_chunk_length,
        data.config.response.max_chunks,
        data.session_skills.clone(),
        data.config.language,
        data.config.discord.owner_id,
        data.turn_participants.clone(),
        data.archived_threads.clone(),
        data.last_tool_name.clone(),
        data.kick_pending.clone(),
        todo_tracker.clone(),
    )
    .await;

    if is_cli_command {
        todo_tracker.lock().await.cleanup(ctx).await;
        data.todo_trackers.lock().await.remove(&thread_id_string);
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
    use super::helpers::{build_context_content, format_cli_command, format_ctx_suffix, is_context_reset_command};
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
    fn no_inject_on_new_command() {
        let result = build_context_content("/new", true, false, "스레드", Lang::Ko);
        assert!(!result.contains("<system-reminder>"));
        assert_eq!(result, "/new");
    }

    #[test]
    fn no_inject_normal_message() {
        let result = build_context_content("일반 메시지", false, false, "스레드", Lang::Ko);
        assert!(!result.contains("<system-reminder>"));
        assert_eq!(result, "일반 메시지");
    }

    #[test]
    fn new_command_case_insensitive() {
        let result = build_context_content("/New", true, false, "스레드", Lang::Ko);
        assert!(!result.contains("<system-reminder>"));
    }

    #[test]
    fn no_inject_on_clear_command() {
        let result = build_context_content("/clear", true, false, "스레드", Lang::Ko);
        assert!(!result.contains("<system-reminder>"));
        assert_eq!(result, "/clear");
    }

    #[test]
    fn clear_command_case_insensitive() {
        let result = build_context_content("/Clear", true, false, "스레드", Lang::Ko);
        assert!(!result.contains("<system-reminder>"));
    }

    #[test]
    fn test_format_ctx_suffix() {
        assert_eq!(format_ctx_suffix(26150, 1000000), " · ctx:2%");
        assert_eq!(format_ctx_suffix(420000, 1000000), " · ctx:42%");
        assert_eq!(format_ctx_suffix(0, 0), "");
        assert_eq!(format_ctx_suffix(100, 0), "");
        assert_eq!(format_ctx_suffix(1000000, 1000000), " · ctx:100%");
    }

    #[test]
    fn context_reset_command_new() {
        assert!(is_context_reset_command("/new"));
        assert!(is_context_reset_command("/New"));
        assert!(is_context_reset_command("/NEW"));
        assert!(is_context_reset_command("  /new  "));
    }

    #[test]
    fn context_reset_command_clear() {
        assert!(is_context_reset_command("/clear"));
        assert!(is_context_reset_command("/Clear"));
        assert!(is_context_reset_command("/CLEAR"));
        assert!(is_context_reset_command("  /clear  "));
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

    #[test]
    fn context_reset_command_rejects_others() {
        assert!(!is_context_reset_command("hello"));
        assert!(!is_context_reset_command("/help"));
        assert!(!is_context_reset_command("/newbie"));
        assert!(!is_context_reset_command("/clearance"));
        assert!(!is_context_reset_command(""));
    }
}
