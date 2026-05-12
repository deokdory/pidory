// ─── T4: Active turn handler ───────────────────────────────────────────────

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use poise::serenity_prelude::{ChannelId, Context, MessageId, UserId};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::mpsc;

use crate::i18n::Lang;
use crate::ratelimit::RateLimitInfo;
use super::super::background::BackgroundTaskTracker;
use super::super::parser::{parse_line, StreamEvent};
use super::super::permission::{PermissionCache, PermissionRequest};
use super::super::session_manager::QueuedMessage;
use super::io::{build_user_message_json, build_interrupt_json, say_silent_chunked};
use super::permission_wait::{wait_for_permissions, PermissionsWaitResult, InitialControlRequest};
use super::ratelimit_bridge::handle_ratelimit_event;

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) async fn run_active_turn(
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
    has_bg_tasks: &Arc<AtomicBool>,
    is_turn_active: &Arc<AtomicBool>,
    last_activity: &Arc<StdMutex<Instant>>,
    pending_recalls: &Arc<tokio::sync::Mutex<HashMap<MessageId, (String, Arc<AtomicBool>)>>>,
    ratelimit_tx: &tokio::sync::watch::Sender<RateLimitInfo>,
    thread_id: &str,
    channel_id: &ChannelId,
    ctx: &Context,
    timeout_secs: u64,
    lang: Lang,
    event_tx: mpsc::Sender<StreamEvent>,
    model_name: &mut String,
    project_path: &Path,
    additional_dirs: &Arc<Vec<PathBuf>>,
) -> bool {
    let mut timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut soft_timeout_fired = false;

    let outer_break: bool = 'turn: loop {
        line.clear();
        tokio::select! {
            // interrupt 요청
            _ = interrupt_rx.recv() => {
                let interrupt_line = build_interrupt_json();
                if let Err(e) = stdin.write_all(interrupt_line.as_bytes()).await {
                    tracing::error!("interrupt write error: {}", e);
                    break 'turn false;
                }
                let _ = stdin.flush().await;
                // result 이벤트는 기존 stdout 읽기에서 처리됨
            }
            // 큐에서 새 메시지 (mid-turn inject)
            new_msg = queue_rx.recv() => {
                match new_msg {
                    Some(m) => {
                        queue_size.fetch_sub(1, Ordering::Relaxed);
                        pending_recalls.lock().await.remove(&m.message_id);
                        if m.cancelled.load(Ordering::Acquire) {
                            tracing::info!(thread_id = %thread_id, msg_id = %m.message_id, "Message recalled, skipping");
                            continue 'turn;
                        }
                        *last_activity.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
                        *current_triggered_by = m.triggered_by;
                        let inject_line = build_user_message_json(&m.content, &m.downloaded_files, m.reply_context.as_ref());
                        if let Err(e) = stdin.write_all(inject_line.as_bytes()).await {
                            tracing::error!("mid-turn stdin write error: {}", e);
                            break 'turn false;
                        }
                        let _ = stdin.flush().await;
                        // m.event_tx는 None이므로 drop됨
                        // 이벤트는 계속 원래 event_tx로 감
                    }
                    None => {
                        // queue closed (kill_session)
                        break 'turn false;
                    }
                }
            }
            // stdout에서 이벤트 읽기
            read_result = tokio::time::timeout_at(timeout_deadline, reader.read_line(line)) => {
                match read_result {
                    Ok(Ok(0)) => {
                        tracing::info!(
                            "Process stdout EOF for thread {}",
                            thread_id
                        );
                        queue_rx.close();
                        break 'turn true;
                    }
                    Ok(Ok(_)) => {
                        timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
                        tracing::debug!(thread_id = %thread_id, "Timeout deadline reset");
                        let trimmed = line.trim_end();
                        if trimmed.is_empty() {
                            continue 'turn;
                        }
                        match parse_line(trimmed) {
                            Ok(event) => {
                                let event_name = match &event {
                                    StreamEvent::Assistant { .. } => "assistant",
                                    StreamEvent::User { .. } => "user",
                                    StreamEvent::Result { .. } => "result",
                                    StreamEvent::ControlRequest { .. } => "control_request",
                                    StreamEvent::RateLimit { .. } => "rate_limit",
                                    StreamEvent::Init { .. } => "init",
                                    _ => "other",
                                };
                                if event_name == "result" || event_name == "other" {
                                    tracing::info!(thread_id = %thread_id, event = event_name, "#36 debug: stdout event");
                                } else {
                                    tracing::debug!(thread_id = %thread_id, event = event_name, "stdout event");
                                }
                                // Background task 이벤트: user turn 중에도 올 수 있음
                                match &event {
                                    StreamEvent::TaskStarted { task_id, task_type, description, .. } => {
                                        tracker.track_started(task_id, task_type, description);
                                        has_bg_tasks.store(tracker.has_active_tasks(), Ordering::Relaxed);
                                        let start_msg = lang.bg_task_started(description);
                                        if let Err(e) = channel_id.say(ctx, &start_msg).await {
                                            tracing::warn!("Failed to send bg task started to Discord: {}", e);
                                        }
                                        continue 'turn;
                                    }
                                    StreamEvent::TaskProgress { task_id, description, .. } => {
                                        tracker.track_progress(task_id, description);
                                        continue 'turn;
                                    }
                                    StreamEvent::TaskNotification { task_id, status, summary, .. } => {
                                        tracker.track_completed(task_id);
                                        has_bg_tasks.store(tracker.has_active_tasks(), Ordering::Relaxed);
                                        let notify_msg = if status == "completed" {
                                            format!("-# 🔔 {}", summary)
                                        } else {
                                            format!("-# ❌ {}", summary)
                                        };
                                        say_silent_chunked(ctx, channel_id, &notify_msg).await;
                                        continue 'turn;
                                    }
                                    _ => {}
                                }

                                // 기존 user turn 이벤트 처리
                                if let StreamEvent::ControlRequest {
                                    ref request_id,
                                    ref tool_name,
                                    ref tool_use_id,
                                    ref input,
                                    ref decision_reason,
                                } = event {
                                    let initial_cr = InitialControlRequest {
                                        request_id: request_id.clone(),
                                        tool_name: tool_name.clone(),
                                        tool_use_id: tool_use_id.clone(),
                                        input: input.clone(),
                                        decision_reason: decision_reason.clone(),
                                        triggered_by: *current_triggered_by,
                                    };
                                    let result = wait_for_permissions(
                                        stdin, reader, line, queue_rx, interrupt_rx,
                                        queue_size, pending_recalls, thread_id,
                                        Some(&event_tx),
                                        ratelimit_tx, permission_cache, permission_tx,
                                        initial_cr,
                                        project_path,
                                        additional_dirs,
                                    ).await;
                                    match result {
                                        PermissionsWaitResult::AllResolved { .. } => {}
                                        PermissionsWaitResult::Interrupted | PermissionsWaitResult::ChannelClosed => {
                                            break 'turn false;
                                        }
                                    }

                                    // timeout 리셋
                                    timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
                                    tracing::debug!(thread_id = %thread_id, "Timeout deadline reset");
                                    continue 'turn;
                                }

                                // 일반 이벤트 처리
                                if let StreamEvent::RateLimit { ref rate_limit_type, utilization, resets_at, is_using_overage, .. } = event {
                                    handle_ratelimit_event(ratelimit_tx, rate_limit_type.as_deref(), utilization, resets_at, is_using_overage);
                                }
                                if let StreamEvent::Init { ref model, .. } = event {
                                    *model_name = model.clone();
                                }
                                let is_result = event.is_result();
                                if is_result {
                                    let sid = event.session_id().unwrap_or("?");
                                    tracing::info!(
                                        thread_id = %thread_id,
                                        session_id = sid,
                                        "#36 debug: user turn Result received — ending turn"
                                    );
                                }
                                let _ = event_tx.send(event).await;
                                if is_result {
                                    is_turn_active.store(false, Ordering::Relaxed);
                                    *last_activity.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
                                    break 'turn false;
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Parse error: {}", e);
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::error!(
                            "stdout read error for thread {}: {}",
                            thread_id,
                            e
                        );
                        queue_rx.close();
                        break 'turn true;
                    }
                    Err(_) => {
                        if !soft_timeout_fired {
                            // Soft timeout: nudge 주입
                            soft_timeout_fired = true;
                            tracing::warn!(
                                thread_id = %thread_id,
                                timeout_secs,
                                "#36 debug: Soft timeout fired — sending nudge"
                            );
                            let nudge_line = build_user_message_json("[SYSTEM] No stdout activity for an extended period. A tool may be unresponsive. Check the status of any running tools and recover if needed.", &[], None);
                            if let Err(e) = stdin.write_all(nudge_line.as_bytes()).await {
                                tracing::error!("nudge write error: {}", e);
                                break 'turn false;
                            }
                            let _ = stdin.flush().await;

                            // Discord 알림 (deadline 설정 전에 처리 — API 지연이 retry window를 잠식하지 않도록)
                            if let Err(e) = channel_id.say(ctx, format!("-# ⚠️ {}", lang.soft_timeout_nudge())).await {
                                tracing::warn!("Failed to send soft timeout nudge to Discord: {}", e);
                            }

                            // 짧은 재대기 (timeout_secs / 5, 최소 60초)
                            let retry_secs = (timeout_secs / 5).max(60);
                            timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(retry_secs);
                            tracing::info!(
                                thread_id = %thread_id,
                                retry_secs,
                                "Nudge sent, retry deadline set"
                            );

                            continue 'turn;
                        } else {
                            // Hard timeout — full session teardown
                            tracing::error!(
                                thread_id = %thread_id,
                                timeout_secs,
                                "Hard turn timeout — killing session"
                            );
                            if let Err(e) = channel_id.say(ctx, format!("⚠️ {}", lang.hard_timeout_kill())).await {
                                tracing::warn!("Failed to send hard timeout message to Discord: {}", e);
                            }
                            queue_rx.close();
                            break 'turn true;
                        }
                    }
                }
            }
        }
    };

    tracing::info!(thread_id = %thread_id, soft_timeout_fired, "Turn ended");
    outer_break
}
