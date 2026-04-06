use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use poise::serenity_prelude::{ChannelId, Context, CreateMessage, FullEvent, GuildId, MessageFlags, MessageId, UserId};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::db::repository;
use crate::error::PidoryError;
use crate::handler::{emoji, formatter, permission_ui};
use crate::handler::emoji::ReactionStatus;
use crate::i18n::Lang;
use crate::subprocess::parser::{ContentBlock, StreamEvent};
use crate::subprocess::permission::PermissionDecision;
use crate::subprocess::session_manager::QueuedMessage;
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
            handle_interaction(ctx, interaction, data).await
        }
        _ => Ok(()),
    }
}

async fn handle_message(
    ctx: &Context,
    new_message: &poise::serenity_prelude::Message,
    data: &Data,
) -> Result<(), PidoryError> {
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
            data.config.discord.owner_id,
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
                data.session_skills.lock().await.remove(&evicted_tid);
                data.needs_context.lock().await.remove(&evicted_tid);
                data.turn_initiators.lock().await.remove(&evicted_tid);
                data.turn_participants.lock().await.remove(&evicted_tid);
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

    let is_new_command = new_message.content.trim().eq_ignore_ascii_case("/new");

    // 원자적 acquire: running이 아닌 경우에만 running으로 전환
    let acquired = repository::try_acquire_session(db, &thread_id).await?;

    if !acquired {
        // mid-turn inject: event_tx 없이 전송 (context inject 안 함, needs_context 소비 안 함)
        let msg = QueuedMessage {
            content: new_message.content.clone(),
            channel_id,
            message_id: msg_id,
            event_tx: None,
            triggered_by: new_message.author.id,
        };

        match data.sessions.send_message(&thread_id, msg).await {
            Ok(()) => {
                // /new가 성공적으로 큐잉된 후에만 flag 세팅
                if is_new_command {
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
                channel_id
                    .say(ctx, format!("❌ {}", lang.queue_full()))
                    .await
                    .map_err(|e| PidoryError::Discord(Box::new(e)))?;
                return Ok(());
            }
            Err(e) => {
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
    let had_needs_context = if !is_new_command {
        data.needs_context.lock().await.remove(&thread_id)
    } else {
        false
    };
    let content = build_context_content(&new_message.content, is_new_session, had_needs_context, &guild_channel.name, lang);

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
    };

    if let Err(e) = data.sessions.send_message(&thread_id, msg).await {
        error!("Failed to send message to session {}: {}", thread_id, e);
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

    // /new가 성공적으로 전송된 후에만 flag 세팅
    if is_new_command {
        data.needs_context.lock().await.insert(thread_id.clone());
    }

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
        data.config.ratelimit.file_path.as_deref(),
        lang,
        data.config.discord.owner_id,
        data.turn_participants.clone(),
    )
    .await;

    Ok(())
}

async fn handle_interaction(
    ctx: &Context,
    interaction: &poise::serenity_prelude::Interaction,
    data: &Data,
) -> Result<(), PidoryError> {
    let component = match interaction {
        poise::serenity_prelude::Interaction::Component(c) => c,
        _ => return Ok(()),
    };

    let (request_id, action) =
        match permission_ui::parse_permission_custom_id(&component.data.custom_id) {
            Some(parsed) => parsed,
            None => return Ok(()),
        };

    let lang = data.config.language;

    // pending HashMap 에서 triggered_by 조회 (consume 하지 않음)
    let triggered_by = {
        let pending = data.pending_permissions.lock().await;
        pending.get(&request_id).map(|p| p.triggered_by)
    };

    let Some(triggered_by) = triggered_by else {
        // 이미 처리되었거나 존재하지 않는 request_id
        return Ok(());
    };

    let is_owner = component.user.id == UserId::new(data.config.discord.owner_id);
    if component.user.id != triggered_by && !is_owner {
        // 비트리거 사용자 — ephemeral 거부, pending 유지, 버튼 활성 유지
        component
            .create_response(
                ctx,
                poise::serenity_prelude::CreateInteractionResponse::Message(
                    poise::serenity_prelude::CreateInteractionResponseMessage::new()
                        .content(format!("❌ {}", lang.no_permission()))
                        .ephemeral(true),
                ),
            )
            .await
            .ok();
        return Ok(());
    }

    // interaction defer — 메시지 업데이트로 응답 (3초 제약)
    component
        .create_response(
            ctx,
            poise::serenity_prelude::CreateInteractionResponse::UpdateMessage(
                poise::serenity_prelude::CreateInteractionResponseMessage::new(),
            ),
        )
        .await
        .ok();

    let decision = match action.as_str() {
        "allow" => PermissionDecision::Allow,
        "always" => PermissionDecision::AlwaysAllow,
        "deny" => PermissionDecision::Deny,
        _ => return Ok(()),
    };

    // pending_permissions에서 꺼내서 oneshot 전송
    let pending = data
        .pending_permissions
        .lock()
        .await
        .remove(&request_id);

    if let Some(p) = pending {
        let tool_name = p.tool_name.clone();
        let message_id = p.message_id;
        // decision 전송 (실패해도 무시)
        let _ = p.response_tx.send(decision);

        // 버튼 disable
        permission_ui::disable_permission_buttons(
            ctx,
            component.channel_id,
            message_id,
            &action,
            &tool_name,
            lang,
        )
        .await
        .ok();
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn process_turn_events(
    ctx: &Context,
    mut event_rx: mpsc::Receiver<StreamEvent>,
    channel_id: ChannelId,
    msg_id: MessageId,
    thread_id: &str,
    db: &sqlx::SqlitePool,
    max_chunk_length: usize,
    max_chunks: usize,
    session_skills: std::sync::Arc<tokio::sync::Mutex<HashMap<String, Vec<String>>>>,
    ratelimit_file: Option<&str>,
    lang: Lang,
    owner_id: u64,
    turn_participants: std::sync::Arc<tokio::sync::Mutex<HashMap<String, std::collections::HashSet<UserId>>>>,
) {
    // turn_participants 에서 멘션 문자열 빌드 (fallback: owner_id)
    let mentions = {
        let parts = turn_participants.lock().await;
        parts.get(thread_id)
            .filter(|set| !set.is_empty())
            .map(|set| set.iter().map(|uid| format!("<@{}>", uid)).collect::<Vec<_>>().join(" "))
            .unwrap_or_else(|| format!("<@{}>", owner_id))
    };

    // 1. typing indicator task 시작
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let typing_paused = Arc::new(AtomicBool::new(false));
    let typing_paused_clone = typing_paused.clone();
    let ctx_clone = ctx.clone();
    let typing_channel = channel_id;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel_clone.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(8)) => {
                    if !typing_paused_clone.load(Ordering::Relaxed) {
                        let _ = typing_channel.broadcast_typing(&ctx_clone).await;
                    }
                }
            }
        }
    });

    // 3. 500ms 빠른 완료 감지
    let mut events: Vec<StreamEvent> = Vec::new();
    let mut got_result = false;
    let mut fast_complete = false;

    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, event_rx.recv()).await {
            Ok(Some(event)) => {
                if event.is_result() {
                    got_result = true;
                }
                events.push(event);
                if got_result {
                    fast_complete = true;
                    break;
                }
            }
            Ok(None) => {
                // sender dropped (worker done or process died)
                fast_complete = true;
                break;
            }
            Err(_) => {
                break; // 500ms timeout
            }
        }
    }

    // 4. 빠른 완료가 아니면 나머지 이벤트 루프
    let mut tool_use_names: HashMap<String, String> = HashMap::new();
    let mut used_tools: Vec<String> = Vec::new();

    if !fast_complete {
        // 버퍼링된 이벤트 먼저 전송
        for event in &events {
            send_event_to_discord(ctx, channel_id, event, &mut tool_use_names, &mut used_tools, max_chunk_length, lang).await;
        }

        loop {
            let stream_event = match event_rx.recv().await {
                Some(e) => Some(Ok(e)),
                None => Some(Err(())),
            };

            match stream_event {
                Some(Ok(stream_event)) => {
                    typing_paused.store(false, Ordering::Relaxed);
                    send_event_to_discord(ctx, channel_id, &stream_event, &mut tool_use_names, &mut used_tools, max_chunk_length, lang).await;

                    if stream_event.is_result() {
                        got_result = true;
                    }
                    let is_result = stream_event.is_result();
                    events.push(stream_event);
                    if is_result {
                        break;
                    }
                }
                Some(Err(())) | None => {
                    // sender dropped
                    break;
                }
            }
        }
    }

    // 5. typing indicator 취소
    cancel.cancel();

    // session_id 추출
    for event in &events {
        if let StreamEvent::Result { session_id, .. } = event
            && !session_id.is_empty() {
            if let Err(e) = repository::update_session_id(db, thread_id, session_id).await {
                warn!("Failed to update session_id: {}", e);
            }
            break;
        }
    }

    // Init skills 캡처
    for event in &events {
        if let StreamEvent::Init { skills, .. } = event {
            if !skills.is_empty() {
                session_skills.lock().await.insert(thread_id.to_string(), skills.clone());
            }
            break;
        }
    }

    // 6. 에러 체크
    let has_cli_error = events.iter().any(|e| e.is_error());
    let is_interrupted = events.iter().any(|e| {
        if let StreamEvent::Result { errors, .. } = e {
            errors.iter().any(|err| err.contains("aborted"))
        } else {
            false
        }
    });

    // 7. 최종 처리
    if fast_complete {
        if is_interrupted {
            if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
            }
            emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Interrupted)
                .await
                .ok();
        } else if has_cli_error || !got_result {
            if let Err(e) = repository::update_session_status(db, thread_id, "error").await {
                tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
            }
            emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
                .await
                .ok();
        } else {
            if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
            }
            emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Done)
                .await
                .ok();
        }
    } else if is_interrupted {
        if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
            tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
        }
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Interrupted)
            .await
            .ok();
    } else if has_cli_error {
        let error_msgs: Vec<String> = events
            .iter()
            .filter_map(|e| {
                if let StreamEvent::Result { is_error, errors, .. } = e {
                    if *is_error {
                        Some(errors.join(", "))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        if let Err(e) = repository::update_session_status(db, thread_id, "error").await {
            tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
        }
        if let Some(error_text) = error_msgs.first()
            && let Err(e) = channel_id.say(ctx, &format!("-# ❌ {} {}", error_text, mentions)).await
        {
            tracing::warn!(%channel_id, "Failed to send turn error notification: {}", e);
        }
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
    } else if !got_result {
        if let Err(e) = repository::update_session_status(db, thread_id, "error").await {
            tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
        }
        if let Err(e) = channel_id.say(ctx, &format!("-# ❌ {} {}", lang.process_abnormal_exit(), mentions)).await {
            tracing::warn!(%channel_id, "Failed to send process exit notification: {}", e);
        }
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
    } else {
        // 정상 완료: 요약 전송
        let (duration_ms, total_cost_usd) = events.iter().find_map(|e| {
            if let StreamEvent::Result { duration_ms, total_cost_usd, .. } = e {
                Some((*duration_ms, *total_cost_usd))
            } else {
                None
            }
        }).unwrap_or((0, 0.0));

        let duration = formatter::format_duration(duration_ms);
        let cost = formatter::format_cost(total_cost_usd);
        let ctx_suffix = format_ctx_suffix(ratelimit_file);
        let summary = if used_tools.is_empty() {
            format!("-# ✅ {}{}{} {}", duration, cost, ctx_suffix, mentions)
        } else {
            used_tools.dedup();
            format!("-# 🔧 {} — {}{}{} {}", used_tools.join(", "), duration, cost, ctx_suffix, mentions)
        };
        if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
            tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
        }
        if let Err(e) = channel_id.say(ctx, &summary).await {
            tracing::warn!(%channel_id, "Failed to send turn summary: {}", e);
        }

        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Done)
            .await
            .ok();
    }

    if let Err(e) = repository::update_last_active(db, thread_id).await {
        tracing::warn!("Failed to update last_active for thread {}: {}", thread_id, e);
    }

    // 8. fast-complete path: 기존 format_response + send_response (한 메시지)
    if fast_complete {
        let response = formatter::format_response(&events, lang);
        let send_ok = if !response.trim().is_empty() {
            match formatter::send_response(ctx, channel_id, &response, max_chunk_length, max_chunks, lang)
                .await
            {
                Ok(()) => true,
                Err(e) => {
                    error!("Failed to send response for thread {}: {}", thread_id, e);
                    false
                }
            }
        } else {
            true
        };

        // 완료 알림 (mention)
        if !is_interrupted {
            if has_cli_error || !got_result || !send_ok {
                if let Err(e) = channel_id.say(ctx, &format!("-# ❌ {} {}", lang.error_occurred(), mentions)).await {
                    tracing::warn!(%channel_id, "Failed to send turn error notification: {}", e);
                }
            } else {
                let (duration_ms, total_cost_usd) = events.iter().find_map(|e| {
                    if let StreamEvent::Result { duration_ms, total_cost_usd, .. } = e {
                        Some((*duration_ms, *total_cost_usd))
                    } else {
                        None
                    }
                }).unwrap_or((0, 0.0));
                let duration = formatter::format_duration(duration_ms);
                let cost = formatter::format_cost(total_cost_usd);
                let ctx_suffix = format_ctx_suffix(ratelimit_file);
                if let Err(e) = channel_id.say(ctx, &format!("-# ✅ {}{}{} {}", duration, cost, ctx_suffix, mentions)).await {
                    tracing::warn!(%channel_id, "Failed to send turn completion notification: {}", e);
                }
            }
        }
    }

}

fn format_ctx_suffix(ratelimit_file: Option<&str>) -> String {
    ratelimit_file
        .and_then(crate::ratelimit::read_ratelimit_file)
        .and_then(|info| info.context_percent)
        .map(|pct| format!(" ctx:{}%", pct))
        .unwrap_or_default()
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

    let is_new_command = content.trim().eq_ignore_ascii_case("/new");

    let acquired = repository::try_acquire_session(db, thread_id).await?;

    if !acquired {
        // mid-turn inject: event_tx 없이 전송
        let msg = QueuedMessage {
            content: content.to_string(),
            channel_id,
            message_id: msg_id,
            event_tx: None,
            triggered_by,
        };
        data.sessions.send_message(thread_id, msg).await?;
        if is_new_command {
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
    emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Running)
        .await
        .ok();

    let (event_tx, event_rx) = mpsc::channel::<StreamEvent>(64);
    let msg = QueuedMessage {
        content: content.to_string(),
        channel_id,
        message_id: msg_id,
        event_tx: Some(event_tx),
        triggered_by,
    };

    // turn_participants 초기화 (skill 직접 실행 경로)
    data.turn_participants
        .lock()
        .await
        .insert(thread_id.to_string(), std::collections::HashSet::from([triggered_by]));

    if let Err(e) = data.sessions.send_message(thread_id, msg).await {
        error!("Failed to send message to session {}: {}", thread_id, e);
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
        repository::update_session_status(db, thread_id, "error").await?;
        return Err(e);
    }

    if is_new_command {
        data.needs_context.lock().await.insert(thread_id.to_string());
    }

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
        data.config.ratelimit.file_path.as_deref(),
        data.config.language,
        data.config.discord.owner_id,
        data.turn_participants.clone(),
    )
    .await;

    Ok(())
}

const DISCORD_MSG_LIMIT: usize = 2000;

async fn say_silent(ctx: &Context, channel_id: ChannelId, content: impl Into<String>) {
    let text = content.into();
    let chunks = if text.chars().count() > DISCORD_MSG_LIMIT {
        formatter::split_message(&text, DISCORD_MSG_LIMIT)
    } else {
        vec![text]
    };
    for chunk in chunks {
        let msg = CreateMessage::new()
            .content(chunk)
            .flags(MessageFlags::SUPPRESS_NOTIFICATIONS);
        if let Err(e) = channel_id.send_message(ctx, msg).await {
            tracing::warn!(%channel_id, "Failed to send message to Discord: {}", e);
        }
    }
}

async fn send_event_to_discord(
    ctx: &Context,
    channel_id: ChannelId,
    event: &StreamEvent,
    tool_use_names: &mut HashMap<String, String>,
    used_tools: &mut Vec<String>,
    max_chunk_length: usize,
    lang: Lang,
) {
    match event {
        StreamEvent::Assistant { content, .. } => {
            for block in content {
                match block {
                    ContentBlock::Text(text) if !text.trim().is_empty() => {
                        let chunks = formatter::split_message(text, max_chunk_length);
                        for chunk in chunks {
                            say_silent(ctx, channel_id, chunk).await;
                        }
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_use_names.insert(id.clone(), name.clone());
                        if !used_tools.contains(name) {
                            used_tools.push(name.clone());
                        }
                        let formatted = formatter::format_tool_use(name, input);
                        let chunks = formatter::split_message(&formatted, max_chunk_length);
                        for chunk in chunks {
                            say_silent(ctx, channel_id, chunk).await;
                        }
                    }
                    _ => {} // Thinking 또는 빈 Text — 무시
                }
            }
        }
        StreamEvent::User { tool_results, .. } => {
            for result in tool_results {
                let tool_name = tool_use_names.get(&result.tool_use_id).map(|s| s.as_str());
                // Read/Grep/Glob 결과는 생략 (에러만 표시)
                if matches!(tool_name, Some("Read" | "Grep" | "Glob")) && !result.is_error {
                    continue;
                }
                if let Some(formatted) = formatter::format_tool_result_with_name(result, tool_name, lang) {
                    say_silent(ctx, channel_id, formatted).await;
                }
            }
        }
        StreamEvent::RateLimit { status, .. } => {
            if status == "rate_limited" {
                say_silent(ctx, channel_id, lang.rate_limit_reached()).await;
            } else if status != "allowed" && !status.is_empty() {
                tracing::warn!(status, "Unknown rate limit status");
            }
        }
        _ => {} // Init, ControlRequest, UserReplay, Result, Unknown — 무시
    }
}

/// 순수 함수: context inject 판정 및 content 생성
fn build_context_content(
    content: &str,
    is_new_session: bool,
    had_needs_context: bool,
    thread_name: &str,
    lang: Lang,
) -> String {
    let is_new_command = content.trim().eq_ignore_ascii_case("/new");
    if !is_new_command && (is_new_session || had_needs_context) {
        let context = lang.session_context(thread_name);
        format!("{}\n\n{}", context, content)
    } else {
        content.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
