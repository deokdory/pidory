// ─── T3: Background turn handler ───────────────────────────────────────────

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use poise::serenity_prelude::{ChannelId, Context, MessageId, UserId};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{Mutex, mpsc};

use crate::config::TimestampConfig;
use crate::db::repository;
use crate::handler::formatter;
use crate::handler::message::{shorten_model_name, format_ctx_suffix};
use crate::handler::session_state::{SessionState, try_acquire_todo_tracker, release_todo_tracker};
use crate::i18n::Lang;
use crate::ratelimit::RateLimitInfo;
use super::super::background::BackgroundTaskTracker;
use super::super::parser::{parse_line, StreamEvent, ContentBlock};
use super::super::permission::{PermissionCache, PermissionRequest};
use super::super::session_manager::QueuedMessage;
use super::io::{say_silent_chunked, build_user_message_json, build_interrupt_json};
use super::permission_wait::{wait_for_permissions, PermissionsWaitResult, InitialControlRequest};
use super::ratelimit_bridge::handle_ratelimit_event;

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) async fn handle_bg_turn(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    line: &mut String,
    queue_rx: &mut mpsc::Receiver<QueuedMessage>,
    interrupt_rx: &mut mpsc::Receiver<()>,
    permission_tx: &mpsc::Sender<PermissionRequest>,
    permission_cache: &mut PermissionCache,
    tracker: &mut BackgroundTaskTracker,
    current_triggered_by: &mut UserId,
    queue_size: &Arc<AtomicUsize>,
    pending_recalls: &Arc<tokio::sync::Mutex<HashMap<MessageId, (String, Arc<AtomicBool>)>>>,
    ratelimit_tx: &tokio::sync::watch::Sender<RateLimitInfo>,
    session_states: &Arc<Mutex<HashMap<String, SessionState>>>,
    thread_id: &str,
    channel_id: &ChannelId,
    ctx: &Context,
    db: &sqlx::PgPool,
    lang: Lang,
    show_context_percent: bool,
    model_name: &mut String,
    project_path: &Path,
    additional_dirs: &Arc<Vec<PathBuf>>,
    timestamp_config: &TimestampConfig,
) {
    let mut used_tools: Vec<String> = Vec::new();
    let mut used_skills: Vec<String> = Vec::new();
    let bg_triggered_by = *current_triggered_by;
    'bg_turn: loop {
        line.clear();
        tokio::select! {
            read_result = reader.read_line(line) => {
                match read_result {
                    Ok(0) => {
                        if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                            tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
                        }
                        break 'bg_turn;
                    }
                    Ok(_) => {
                        let trimmed = line.trim_end();
                        if trimmed.is_empty() { continue 'bg_turn; }
                        match parse_line(trimmed) {
                            Ok(StreamEvent::Result { duration_ms, total_cost_usd, input_tokens, output_tokens, context_window, total_input_tokens, is_error, ref errors, .. }) => {
                                let is_interrupted = errors.iter().any(|err| err.contains("aborted"));
                                let duration = formatter::format_duration(duration_ms);
                                let cost = formatter::format_cost(total_cost_usd);
                                let tokens = formatter::format_tokens(input_tokens, output_tokens);
                                let ctx_suffix = format_ctx_suffix(total_input_tokens, context_window, show_context_percent);
                                let model_short = if model_name.is_empty() {
                                    String::new()
                                } else {
                                    shorten_model_name(model_name)
                                };
                                let model_part = if model_short.is_empty() { String::new() } else { format!("**{}**", model_short) };
                                let parts: Vec<&str> = [model_part.as_str(), duration.as_str(), cost.as_str(), tokens.as_str()]
                                    .iter()
                                    .filter(|s| !s.is_empty())
                                    .copied()
                                    .collect();
                                let stats = parts.join(" · ");
                                let stats_line = format!("-# {}{}", stats, ctx_suffix);
                                let mention = format!("<@{}>", bg_triggered_by);
                                let icon = if is_interrupted { "⏹️" } else if is_error { "❌" } else { "✅" };
                                let tools_line = if used_tools.is_empty() {
                                    String::new()
                                } else {
                                    let tools_str = used_tools.iter().map(|t| formatter::inline_code(t)).collect::<Vec<_>>().join(", ");
                                    format!("\n-# Tools: {}", tools_str)
                                };
                                let skills_line = if used_skills.is_empty() {
                                    String::new()
                                } else {
                                    used_skills.dedup();
                                    format!("\n-# Skills: {}", used_skills.iter().map(|s| formatter::inline_code(s)).collect::<Vec<_>>().join(", "))
                                };
                                let summary = format!("-# {} {}\n{}{}{}", icon, mention, stats_line, tools_line, skills_line);
                                // bg turn 종료 — pending TodoWrite flush
                                if let Some(mut todo_tracker) = try_acquire_todo_tracker(session_states, thread_id, *channel_id).await {
                                    todo_tracker.flush(ctx).await;
                                    release_todo_tracker(session_states, thread_id, todo_tracker, ctx).await;
                                } else {
                                    tracing::warn!("bg flush: tracker unavailable (race with primary turn or session gone)");
                                }
                                if is_error && !is_interrupted {
                                    let error_msg = errors.join(", ");
                                    if !error_msg.is_empty() {
                                        say_silent_chunked(ctx, channel_id, &format!("-# ❌ {}", error_msg)).await;
                                    }
                                }
                                say_silent_chunked(ctx, channel_id, &summary).await;
                                let new_status = if is_error && !is_interrupted { "error" } else { "idle" };
                                if let Err(e) = repository::update_session_status(db, thread_id, new_status).await {
                                    tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
                                }
                                break 'bg_turn;
                            }
                            Ok(StreamEvent::Assistant { ref content, .. }) => {
                                for block in content {
                                    match block {
                                        ContentBlock::Text(text) if !text.trim().is_empty() => {
                                            let bg_text = lang.bg_notification(text);
                                            say_silent_chunked(ctx, channel_id, &bg_text).await;
                                        }
                                        ContentBlock::ToolUse { name, input, .. } => {
                                            if name == "Skill" {
                                                if let Some(skill_name) = input.get("skill").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                                                    if !used_skills.iter().any(|s| s == skill_name) {
                                                        used_skills.push(skill_name.to_owned());
                                                    }
                                                } else if !used_tools.contains(name) {
                                                    used_tools.push(name.clone());
                                                }
                                            } else if !used_tools.contains(name) {
                                                used_tools.push(name.clone());
                                            }
                                            if name == "TodoWrite" {
                                                // try_acquire가 None이면 silent skip + warn (primary와 race 또는 cleanup된 세션)
                                                if let Some(mut todo_tracker) = try_acquire_todo_tracker(session_states, thread_id, *channel_id).await {
                                                    todo_tracker.update(ctx, input).await;
                                                    release_todo_tracker(session_states, thread_id, todo_tracker, ctx).await;
                                                } else {
                                                    tracing::warn!("bg TodoWrite: tracker unavailable (race with primary turn or session gone)");
                                                }
                                            } else {
                                                let formatted = formatter::format_tool_use(name, input);
                                                let bg_text = lang.bg_notification(&formatted);
                                                say_silent_chunked(ctx, channel_id, &bg_text).await;
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Ok(StreamEvent::User { ref tool_results, .. }) => {
                                for result in tool_results {
                                    if result.is_error
                                        && let Some(formatted) = formatter::format_tool_result_with_name(result, None, lang)
                                    {
                                        let bg_text = lang.bg_notification(&formatted);
                                        say_silent_chunked(ctx, channel_id, &bg_text).await;
                                    }
                                }
                            }
                            Ok(StreamEvent::ControlRequest { ref request_id, ref tool_name, ref tool_use_id, ref input, ref decision_reason, .. }) => {
                                let initial_cr = InitialControlRequest {
                                    request_id: request_id.clone(),
                                    tool_name: tool_name.clone(),
                                    tool_use_id: tool_use_id.clone(),
                                    input: input.clone(),
                                    decision_reason: decision_reason.clone(),
                                    triggered_by: bg_triggered_by,
                                };
                                let result = wait_for_permissions(
                                    stdin, reader, line, queue_rx, interrupt_rx,
                                    queue_size, pending_recalls, thread_id,
                                    None,
                                    ratelimit_tx, permission_cache, permission_tx,
                                    initial_cr,
                                    project_path,
                                    additional_dirs,
                                    timestamp_config,
                                ).await;
                                match result {
                                    PermissionsWaitResult::AllResolved { .. } => {}
                                    PermissionsWaitResult::Interrupted | PermissionsWaitResult::ChannelClosed => {
                                        if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                                            tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
                                        }
                                        break 'bg_turn;
                                    }
                                }
                            }
                            Ok(StreamEvent::Init { ref model, .. }) => {
                                *model_name = model.clone();
                            }
                            Ok(StreamEvent::TaskStarted { ref task_id, ref task_type, ref description, .. }) => {
                                tracker.track_started(task_id, task_type, description);
                            }
                            Ok(StreamEvent::TaskProgress { ref task_id, ref description, .. }) => {
                                tracker.track_progress(task_id, description);
                            }
                            Ok(StreamEvent::RateLimit { rate_limit_type, utilization, resets_at, is_using_overage, .. }) => {
                                handle_ratelimit_event(ratelimit_tx, rate_limit_type.as_deref(), utilization, resets_at, is_using_overage);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("Parse error (bg turn): {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("stdout read error (bg turn): {}", e);
                        if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                            tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
                        }
                        break 'bg_turn;
                    }
                }
            }
            // bg turn 중 queue 메시지 → mid-turn inject
            new_msg = queue_rx.recv() => {
                match new_msg {
                    Some(m) => {
                        queue_size.fetch_sub(1, Ordering::Relaxed);
                        pending_recalls.lock().await.remove(&m.message_id);
                        if m.cancelled.load(Ordering::Acquire) {
                            tracing::info!(thread_id = %thread_id, msg_id = %m.message_id, "Message recalled, skipping");
                            continue 'bg_turn;
                        }
                        *current_triggered_by = m.triggered_by;
                        let inject_line = build_user_message_json(&m.content, &m.downloaded_files, m.reply_context.as_ref(), m.sender_info.as_ref(), timestamp_config, chrono::Utc::now());
                        if let Err(e) = stdin.write_all(inject_line.as_bytes()).await {
                            tracing::error!("mid-turn stdin write error (bg turn): {}", e);
                            if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                                tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
                            }
                            break 'bg_turn;
                        }
                        let _ = stdin.flush().await;
                    }
                    None => {
                        if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                            tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
                        }
                        break 'bg_turn;
                    }
                }
            }
            // bg turn 중 interrupt
            _ = interrupt_rx.recv() => {
                let interrupt_line = build_interrupt_json();
                if let Err(e) = stdin.write_all(interrupt_line.as_bytes()).await {
                    tracing::error!("interrupt write error (bg turn): {}", e);
                } else {
                    let _ = stdin.flush().await;
                }
            }
        }
    }
}
