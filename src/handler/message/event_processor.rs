use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use poise::serenity_prelude::{ChannelId, Context, CreateMessage, MessageFlags, MessageId, UserId};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::db::repository;
use crate::handler::{emoji, file_attach, formatter};
use crate::handler::emoji::ReactionStatus;
use crate::handler::message::helpers::shorten_model_name;
use crate::handler::status::ProgressIndicator;
use crate::i18n::Lang;
use crate::subprocess::parser::{ContentBlock, StreamEvent};

use super::helpers::format_ctx_suffix;

const DISCORD_MSG_LIMIT: usize = 2000;

pub(super) async fn say_silent(ctx: &Context, channel_id: ChannelId, content: impl Into<String>) {
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

pub(super) async fn send_event_to_discord(
    ctx: &Context,
    channel_id: ChannelId,
    event: &StreamEvent,
    tool_use_names: &mut HashMap<String, String>,
    used_tools: &mut Vec<String>,
    max_chunk_length: usize,
    lang: Lang,
    last_tool_name: &std::sync::Arc<tokio::sync::Mutex<HashMap<String, String>>>,
    thread_id: &str,
) {
    match event {
        StreamEvent::Assistant { content, .. } => {
            for block in content {
                match block {
                    ContentBlock::Text(text) if !text.trim().is_empty() => {
                        let (clean_text, file_paths) = file_attach::extract_file_markers(text);
                        if !clean_text.trim().is_empty() {
                            let converted = formatter::convert_markdown_tables(&clean_text);
                            let chunks = formatter::split_message(&converted, max_chunk_length);
                            for chunk in chunks {
                                say_silent(ctx, channel_id, chunk).await;
                            }
                        }
                        if !file_paths.is_empty()
                            && let Err(e) = file_attach::send_file_attachments(ctx, channel_id, &file_paths, lang).await
                        {
                            error!("Failed to send file attachments: {}", e);
                        }
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_use_names.insert(id.clone(), name.clone());
                        if !used_tools.contains(name) {
                            used_tools.push(name.clone());
                        }
                        {
                            let mut map = last_tool_name.lock().await;
                            if let Some(existing) = map.get_mut(thread_id) {
                                existing.clone_from(name);
                            } else {
                                map.insert(thread_id.to_string(), name.clone());
                            }
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
                // 조회/편집/검색 도구 결과는 생략 (에러만 표시)
                if formatter::is_noise_tool(tool_name) && !result.is_error {
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
    lang: Lang,
    owner_id: u64,
    turn_participants: std::sync::Arc<tokio::sync::Mutex<HashMap<String, std::collections::HashSet<UserId>>>>,
    archived_threads: std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
    last_tool_name: std::sync::Arc<tokio::sync::Mutex<HashMap<String, String>>>,
    kick_pending: std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
) {
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
            send_event_to_discord(ctx, channel_id, event, &mut tool_use_names, &mut used_tools, max_chunk_length, lang, &last_tool_name, thread_id).await;
        }

        // Progress indicator 초기화
        let mut progress = ProgressIndicator::new(channel_id, lang);
        let mut tick_interval = tokio::time::interval(Duration::from_secs(1));
        tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                event = event_rx.recv() => {
                    match event {
                        Some(stream_event) => {
                            // Progress 상태 업데이트 (send_event_to_discord 전에!)
                            if stream_event.is_control_request() {
                                progress.on_control_request();
                            } else {
                                if progress.is_paused() {
                                    progress.on_resume();
                                }

                                if let StreamEvent::Assistant { content, .. } = &stream_event {
                                    let has_tool_use = content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }));
                                    if has_tool_use {
                                        for block in content {
                                            if let ContentBlock::ToolUse { name, .. } = block {
                                                progress.on_tool_use(name, ctx).await;
                                            }
                                        }
                                    } else {
                                        // Text-only assistant → thinking timer reset
                                        progress.on_event();
                                    }
                                } else if matches!(&stream_event, StreamEvent::User { .. }) {
                                    progress.on_tool_result(ctx).await;
                                } else if !stream_event.is_result() {
                                    progress.on_event();
                                }
                            }

                            // typing indicator 제어
                            typing_paused.store(progress.is_active(), Ordering::Relaxed);

                            // 기존 이벤트 처리
                            send_event_to_discord(ctx, channel_id, &stream_event, &mut tool_use_names, &mut used_tools, max_chunk_length, lang, &last_tool_name, thread_id).await;

                            if stream_event.is_result() {
                                got_result = true;
                            }
                            let is_result = stream_event.is_result();
                            events.push(stream_event);
                            if is_result {
                                break;
                            }
                        }
                        None => {
                            // sender dropped
                            break;
                        }
                    }
                }
                _ = tick_interval.tick() => {
                    progress.tick(ctx).await;
                    typing_paused.store(progress.is_active(), Ordering::Relaxed);
                }
            }
        }

        // Turn 종료 시 cleanup
        progress.cleanup(ctx).await;
    }

    // 5. typing indicator 취소
    cancel.cancel();

    // session_id 추출
    for event in &events {
        if let StreamEvent::Result { session_id, .. } = event
            && !session_id.is_empty() {
            if let Err(e) = repository::update_session_id(db, thread_id, session_id).await {
                tracing::warn!("Failed to update session_id: {}", e);
            }
            break;
        }
    }

    // Init skills + model 캡처
    let mut turn_model = String::new();
    for event in &events {
        if let StreamEvent::Init { skills, model, .. } = event {
            if !skills.is_empty() {
                session_skills.lock().await.insert(thread_id.to_string(), skills.clone());
            }
            turn_model = shorten_model_name(model);
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

    if !is_interrupted {
        kick_pending.lock().await.remove(thread_id);
    }

    if archived_threads.lock().await.remove(thread_id) {
        tracing::info!(thread_id, "Turn ended silently — thread archived");
        return;
    }

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
        let mentions = {
            let parts = turn_participants.lock().await;
            parts.get(thread_id)
                .filter(|set| !set.is_empty())
                .map(|set| set.iter().map(|uid| format!("<@{}>", uid)).collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|| format!("<@{}>", owner_id))
        };
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
        let mentions = {
            let parts = turn_participants.lock().await;
            parts.get(thread_id)
                .filter(|set| !set.is_empty())
                .map(|set| set.iter().map(|uid| format!("<@{}>", uid)).collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|| format!("<@{}>", owner_id))
        };
        if let Err(e) = channel_id.say(ctx, &format!("-# ❌ {} {}", lang.process_abnormal_exit(), mentions)).await {
            tracing::warn!(%channel_id, "Failed to send process exit notification: {}", e);
        }
        emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error)
            .await
            .ok();
    } else {
        // 정상 완료: 요약 전송
        let (duration_ms, total_cost_usd, input_tokens, output_tokens, context_window, total_input_tokens) = events.iter().find_map(|e| {
            if let StreamEvent::Result { duration_ms, total_cost_usd, input_tokens, output_tokens, context_window, total_input_tokens, .. } = e {
                Some((*duration_ms, *total_cost_usd, *input_tokens, *output_tokens, *context_window, *total_input_tokens))
            } else {
                None
            }
        }).unwrap_or((0, 0.0, 0, 0, 0, 0));
        let duration = formatter::format_duration(duration_ms);
        let cost = formatter::format_cost(total_cost_usd);
        let tokens = formatter::format_tokens(input_tokens, output_tokens);
        let ctx_suffix = format_ctx_suffix(total_input_tokens, context_window);
        let mentions = {
            let parts = turn_participants.lock().await;
            parts.get(thread_id)
                .filter(|set| !set.is_empty())
                .map(|set| set.iter().map(|uid| format!("<@{}>", uid)).collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|| format!("<@{}>", owner_id))
        };
        let model_part = if turn_model.is_empty() { String::new() } else { format!("**{}**", turn_model) };
        let stats_line = format!("-# {} · {} · {} · {}{}", model_part, duration, cost, tokens, ctx_suffix);
        let summary = if used_tools.is_empty() {
            format!("-# ✅ {}\n{}", mentions, stats_line)
        } else {
            used_tools.dedup();
            format!("-# ✅ {}\n{}\n-# Tools: {}", mentions, stats_line, used_tools.iter().map(|t| formatter::inline_code(t)).collect::<Vec<_>>().join(", "))
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
        let (response, file_paths) = formatter::format_response(&events, lang);
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
        if !file_paths.is_empty()
            && let Err(e) = file_attach::send_file_attachments(ctx, channel_id, &file_paths, lang).await
        {
            error!("Failed to send file attachments: {}", e);
        }

        // 완료 알림 (mention)
        if !is_interrupted {
            let mentions = {
                let parts = turn_participants.lock().await;
                parts.get(thread_id)
                    .filter(|set| !set.is_empty())
                    .map(|set| set.iter().map(|uid| format!("<@{}>", uid)).collect::<Vec<_>>().join(" "))
                    .unwrap_or_else(|| format!("<@{}>", owner_id))
            };
            if has_cli_error || !got_result || !send_ok {
                if let Err(e) = channel_id.say(ctx, &format!("-# ❌ {} {}", lang.error_occurred(), mentions)).await {
                    tracing::warn!(%channel_id, "Failed to send turn error notification: {}", e);
                }
            } else {
                let (duration_ms, total_cost_usd, input_tokens, output_tokens, context_window, total_input_tokens) = events.iter().find_map(|e| {
                    if let StreamEvent::Result { duration_ms, total_cost_usd, input_tokens, output_tokens, context_window, total_input_tokens, .. } = e {
                        Some((*duration_ms, *total_cost_usd, *input_tokens, *output_tokens, *context_window, *total_input_tokens))
                    } else {
                        None
                    }
                }).unwrap_or((0, 0.0, 0, 0, 0, 0));
                let duration = formatter::format_duration(duration_ms);
                let cost = formatter::format_cost(total_cost_usd);
                let tokens = formatter::format_tokens(input_tokens, output_tokens);
                let ctx_suffix = format_ctx_suffix(total_input_tokens, context_window);
                let model_part = if turn_model.is_empty() { String::new() } else { format!("**{}**", turn_model) };
                let stats_line = format!("-# {} · {} · {} · {}{}", model_part, duration, cost, tokens, ctx_suffix);
                if let Err(e) = channel_id.say(ctx, &format!("-# ✅ {}\n{}", mentions, stats_line)).await {
                    tracing::warn!(%channel_id, "Failed to send turn completion notification: {}", e);
                }
            }
        }
    }

}
