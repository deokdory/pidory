use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use poise::serenity_prelude::{ChannelId, Context, FullEvent, GuildId, MessageId, UserId};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::db::repository;
use crate::error::PidoryError;
use crate::handler::{emoji, formatter, permission_ui};
use crate::handler::emoji::ReactionStatus;
use crate::subprocess::parser::{ContentBlock, StreamEvent};
use crate::subprocess::permission::{PermissionDecision, PermissionRequest};
use crate::subprocess::session_manager::QueuedMessage;
use crate::{Data, PendingPermission};

pub async fn handle_event(
    ctx: &Context,
    event: &FullEvent,
    data: &Data,
) -> Result<(), PidoryError> {
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

    // owner ID 검증
    if new_message.author.id != UserId::new(data.config.discord.owner_id) {
        return Ok(());
    }

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
    let channel_id = new_message.channel_id;
    let msg_id = new_message.id;

    // 세션 DB 확인/생성
    let session = match repository::get_session_by_thread(db, &thread_id).await? {
        Some(s) => s,
        None => {
            tracing::info!("Creating new session for thread {}", thread_id);
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
        )
        .await
    {
        Ok(Some(rx)) => {
            // 새 세션 — permission_rx 보관
            data.permission_rxs.lock().await.insert(thread_id.clone(), rx);
        }
        Ok(None) => {
            // 기존 세션 — permission_rx 이미 보관됨
        }
        Err(e) => {
            error!("Failed to get_or_create session for thread {}: {}", thread_id, e);
            channel_id
                .say(ctx, format!("❌ 세션 생성 실패: {}", e))
                .await
                .map_err(|e| PidoryError::Discord(Box::new(e)))?;
            return Ok(());
        }
    }

    // 원자적 acquire: running이 아닌 경우에만 running으로 전환
    let acquired = repository::try_acquire_session(db, &thread_id).await?;

    if !acquired {
        // mid-turn inject: event_tx 없이 전송
        let msg = QueuedMessage {
            content: new_message.content.clone(),
            channel_id,
            message_id: msg_id,
            event_tx: None,
        };

        match data.sessions.send_message(&thread_id, msg).await {
            Ok(()) => {
                let _ = channel_id
                    .create_reaction(
                        ctx,
                        msg_id,
                        poise::serenity_prelude::ReactionType::Unicode("✅".to_string()),
                    )
                    .await;
            }
            Err(e) if e.to_string().contains("queue full") => {
                channel_id
                    .say(ctx, "❌ 대기열이 가득 찼습니다")
                    .await
                    .map_err(|e| PidoryError::Discord(Box::new(e)))?;
                return Ok(());
            }
            Err(e) => {
                channel_id
                    .say(ctx, format!("❌ 오류: {}", e))
                    .await
                    .map_err(|e| PidoryError::Discord(Box::new(e)))?;
                return Ok(());
            }
        }

        return Ok(());
    }

    // 직접 실행 경로
    emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Running)
        .await
        .ok();

    let (event_tx, event_rx) = mpsc::channel::<StreamEvent>(64);
    let msg = QueuedMessage {
        content: new_message.content.clone(),
        channel_id,
        message_id: msg_id,
        event_tx: Some(event_tx),
    };

    if let Err(e) = data.sessions.send_message(&thread_id, msg).await {
        error!("Failed to send message to session {}: {}", thread_id, e);
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
        repository::update_session_status(db, &thread_id, "error").await?;
        channel_id
            .say(ctx, format!("❌ 메시지 전송 실패: {}", e))
            .await
            .map_err(|e| PidoryError::Discord(Box::new(e)))?;
        return Ok(());
    }

    // permission_rx를 꺼내서 process_turn_events에 넘김 (turn 종료 후 다시 넣음)
    let permission_rx = data.permission_rxs.lock().await.remove(&thread_id);

    let permission_rx = process_turn_events(
        ctx,
        event_rx,
        channel_id,
        msg_id,
        &thread_id,
        db,
        data.config.response.max_chunk_length,
        data.config.response.max_chunks,
        permission_rx,
        data.pending_permissions.clone(),
        data.config.discord.owner_id,
        data.session_skills.clone(),
    )
    .await;

    // permission_rx를 다시 보관 (다음 turn에서 사용)
    if let Some(rx) = permission_rx {
        data.permission_rxs.lock().await.insert(thread_id.clone(), rx);
    }

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

    // owner 검증
    if component.user.id != UserId::new(data.config.discord.owner_id) {
        component
            .create_response(
                ctx,
                poise::serenity_prelude::CreateInteractionResponse::Message(
                    poise::serenity_prelude::CreateInteractionResponseMessage::new()
                        .content("❌ 권한이 없습니다")
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
    mut permission_rx: Option<mpsc::Receiver<PermissionRequest>>,
    pending_permissions: std::sync::Arc<tokio::sync::Mutex<HashMap<String, PendingPermission>>>,
    owner_id: u64,
    session_skills: std::sync::Arc<tokio::sync::Mutex<HashMap<String, Vec<String>>>>,
) -> Option<mpsc::Receiver<PermissionRequest>> {
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
            send_event_to_discord(ctx, channel_id, event, &mut tool_use_names, &mut used_tools).await;
        }

        loop {
            // permission_rx가 있으면 select, 없으면 event_rx만
            let stream_event = if permission_rx.is_some() {
                tokio::select! {
                    ev = event_rx.recv() => {
                        match ev {
                            Some(e) => Some(Ok(e)),
                            None => Some(Err(())),
                        }
                    }
                    perm = async {
                        if let Some(rx) = permission_rx.as_mut() {
                            rx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        if let Some(perm_req) = perm {
                            typing_paused.store(true, Ordering::Relaxed);
                            handle_permission_request(
                                ctx,
                                channel_id,
                                perm_req,
                                &pending_permissions,
                                owner_id,
                            )
                            .await;
                        }
                        continue;
                    }
                }
            } else {
                match event_rx.recv().await {
                    Some(e) => Some(Ok(e)),
                    None => Some(Err(())),
                }
            };

            match stream_event {
                Some(Ok(stream_event)) => {
                    typing_paused.store(false, Ordering::Relaxed);
                    send_event_to_discord(ctx, channel_id, &stream_event, &mut tool_use_names, &mut used_tools).await;

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

    // 7. 최종 처리
    if fast_complete {
        if has_cli_error || !got_result {
            emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
                .await
                .ok();
            repository::update_session_status(db, thread_id, "error")
                .await
                .ok();
        } else {
            emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Done)
                .await
                .ok();
            repository::update_session_status(db, thread_id, "idle")
                .await
                .ok();
        }
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
        if let Some(error_text) = error_msgs.first() {
            channel_id.say(ctx, &format!("-# ❌ {}", error_text)).await.ok();
        }
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
        repository::update_session_status(db, thread_id, "error")
            .await
            .ok();
    } else if !got_result {
        channel_id.say(ctx, "-# ❌ 프로세스가 비정상 종료되었습니다").await.ok();
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
        repository::update_session_status(db, thread_id, "error")
            .await
            .ok();
    } else {
        // 정상 완료: 요약 전송
        let duration_ms = events.iter().find_map(|e| {
            if let StreamEvent::Result { duration_ms, .. } = e {
                Some(*duration_ms)
            } else {
                None
            }
        }).unwrap_or(0);

        let duration = formatter::format_duration(duration_ms);
        let summary = if used_tools.is_empty() {
            format!("-# {}", duration)
        } else {
            used_tools.dedup();
            format!("-# 🔧 {} — {}", used_tools.join(", "), duration)
        };
        channel_id.say(ctx, &summary).await.ok();

        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Done)
            .await
            .ok();
        repository::update_session_status(db, thread_id, "idle")
            .await
            .ok();
    }

    repository::update_last_active(db, thread_id).await.ok();

    // 8. fast-complete path: 기존 format_response + send_response (한 메시지)
    if fast_complete {
        let response = formatter::format_response(&events);
        if !response.trim().is_empty()
            && let Err(e) =
                formatter::send_response(ctx, channel_id, &response, max_chunk_length, max_chunks)
                    .await
        {
            error!("Failed to send response for thread {}: {}", thread_id, e);
        }
    }

    permission_rx
}

pub async fn execute_in_session(
    ctx: &Context,
    data: &Data,
    thread_id: &str,
    channel_id: ChannelId,
    msg_id: MessageId,
    content: &str,
) -> Result<(), PidoryError> {
    let db = &data.db;

    let acquired = repository::try_acquire_session(db, thread_id).await?;

    if !acquired {
        // mid-turn inject: event_tx 없이 전송
        let msg = QueuedMessage {
            content: content.to_string(),
            channel_id,
            message_id: msg_id,
            event_tx: None,
        };
        data.sessions.send_message(thread_id, msg).await?;
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
    };

    if let Err(e) = data.sessions.send_message(thread_id, msg).await {
        error!("Failed to send message to session {}: {}", thread_id, e);
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
        repository::update_session_status(db, thread_id, "error").await?;
        return Err(e);
    }

    let permission_rx = data.permission_rxs.lock().await.remove(thread_id);

    let permission_rx = process_turn_events(
        ctx,
        event_rx,
        channel_id,
        msg_id,
        thread_id,
        db,
        data.config.response.max_chunk_length,
        data.config.response.max_chunks,
        permission_rx,
        data.pending_permissions.clone(),
        data.config.discord.owner_id,
        data.session_skills.clone(),
    )
    .await;

    if let Some(rx) = permission_rx {
        data.permission_rxs.lock().await.insert(thread_id.to_string(), rx);
    }

    Ok(())
}

async fn send_event_to_discord(
    ctx: &Context,
    channel_id: ChannelId,
    event: &StreamEvent,
    tool_use_names: &mut HashMap<String, String>,
    used_tools: &mut Vec<String>,
) {
    match event {
        StreamEvent::Assistant { content, .. } => {
            for block in content {
                match block {
                    ContentBlock::Text(text) if !text.trim().is_empty() => {
                        channel_id.say(ctx, text).await.ok();
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_use_names.insert(id.clone(), name.clone());
                        if !used_tools.contains(name) {
                            used_tools.push(name.clone());
                        }
                        let formatted = formatter::format_tool_use(name, input);
                        channel_id.say(ctx, &formatted).await.ok();
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
                if let Some(formatted) = formatter::format_tool_result_with_name(result, tool_name) {
                    channel_id.say(ctx, &formatted).await.ok();
                }
            }
        }
        StreamEvent::RateLimit { status, .. } => {
            if status != "allowed" {
                channel_id.say(ctx, "⚠️ Rate limit reached").await.ok();
            }
        }
        _ => {} // Init, ControlRequest, UserReplay, Result, Unknown — 무시
    }
}

async fn handle_permission_request(
    ctx: &Context,
    channel_id: ChannelId,
    perm_req: PermissionRequest,
    pending_permissions: &std::sync::Arc<tokio::sync::Mutex<HashMap<String, PendingPermission>>>,
    _owner_id: u64,
) {
    let msg = permission_ui::create_permission_message(
        &perm_req.tool_name,
        &perm_req.input,
        &perm_req.request_id,
        perm_req.decision_reason.as_deref(),
    );

    match channel_id.send_message(ctx, msg).await {
        Ok(sent) => {
            let pending = PendingPermission {
                response_tx: perm_req.response_tx,
                tool_name: perm_req.tool_name,
                message_id: sent.id,
            };
            pending_permissions
                .lock()
                .await
                .insert(perm_req.request_id, pending);
        }
        Err(e) => {
            warn!("Failed to send permission message: {}", e);
            // 전송 실패 시 deny
            let _ = perm_req
                .response_tx
                .send(PermissionDecision::Deny);
        }
    }
}
