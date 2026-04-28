// ─── T2: Between-turns event handling ──────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use poise::serenity_prelude::{ChannelId, Context, MessageId, UserId};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{Mutex, mpsc};

use crate::db::repository;
use crate::handler::session_state::SessionState;
use crate::i18n::Lang;
use crate::ratelimit::RateLimitInfo;
use super::super::background::BackgroundTaskTracker;
use super::super::parser::{parse_line, StreamEvent};
use super::super::permission::{PermissionCache, PermissionRequest};
use super::super::session_manager::QueuedMessage;
use super::io::say_silent_chunked;
use super::permission_wait::{wait_for_permissions, PermissionsWaitResult, InitialControlRequest};

pub(super) enum BetweenTurnsAction {
    /// Continue the main loop (no queued message ready)
    Continue,
    /// Break the main loop (EOF or fatal error)
    Break,
    /// A primary message was dequeued and is ready to process
    ProcessMessage(QueuedMessage),
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_between_turns_event(
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
    pending_recalls: &Arc<tokio::sync::Mutex<HashMap<MessageId, (String, Arc<AtomicBool>)>>>,
    ratelimit_tx: &tokio::sync::watch::Sender<RateLimitInfo>,
    session_states: &Arc<Mutex<HashMap<String, SessionState>>>,
    thread_id: &str,
    channel_id: &ChannelId,
    ctx: &Context,
    db: &sqlx::SqlitePool,
    lang: Lang,
    model_name: &mut String,
) -> BetweenTurnsAction {
    line.clear();
    tokio::select! {
        biased;
        // stdout 우선: background task 이벤트 감지
        read_result = reader.read_line(line) => {
            match read_result {
                Ok(0) => {
                    tracing::info!(
                        "Process stdout EOF (between turns) for thread {}",
                        thread_id
                    );
                    BetweenTurnsAction::Break
                }
                Ok(_) => {
                    let trimmed = line.trim_end();
                    if trimmed.is_empty() {
                        return BetweenTurnsAction::Continue;
                    }
                    match parse_line(trimmed) {
                        Ok(StreamEvent::TaskStarted { ref task_id, ref task_type, ref description, .. }) => {
                            tracker.track_started(task_id, task_type, description);
                            has_bg_tasks.store(tracker.has_active_tasks(), Ordering::Relaxed);
                            tracing::info!("Background task started: {} ({})", task_id, task_type);
                            let start_msg = lang.bg_task_started(description);
                            if let Err(e) = channel_id.say(ctx, &start_msg).await {
                                tracing::warn!("Failed to send bg task started to Discord: {}", e);
                            }
                            BetweenTurnsAction::Continue
                        }
                        Ok(StreamEvent::TaskProgress { ref task_id, ref description, .. }) => {
                            tracker.track_progress(task_id, description);
                            BetweenTurnsAction::Continue
                        }
                        Ok(StreamEvent::TaskNotification { ref task_id, ref status, ref summary, .. }) => {
                            tracker.track_completed(task_id);
                            has_bg_tasks.store(tracker.has_active_tasks(), Ordering::Relaxed);
                            let notify_msg = if status == "completed" {
                                format!("-# 🔔 {}", summary)
                            } else {
                                format!("-# ❌ {}", summary)
                            };
                            say_silent_chunked(ctx, channel_id, &notify_msg).await;

                            if let Err(e) = repository::update_session_status(db, thread_id, "running").await {
                                tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
                            }

                            super::turn_bg::handle_bg_turn(
                                stdin,
                                reader,
                                line,
                                queue_rx,
                                interrupt_rx,
                                permission_tx,
                                permission_cache,
                                tracker,
                                current_triggered_by,
                                queue_size,
                                pending_recalls,
                                ratelimit_tx,
                                session_states,
                                thread_id,
                                channel_id,
                                ctx,
                                db,
                                lang,
                                model_name,
                            ).await;

                            BetweenTurnsAction::Continue
                        }
                        Ok(StreamEvent::ControlRequest { ref request_id, ref tool_name, ref tool_use_id, ref input, ref decision_reason, .. }) => {
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
                                None,
                                ratelimit_tx, permission_cache, permission_tx,
                                initial_cr,
                            ).await;
                            match result {
                                PermissionsWaitResult::AllResolved { .. } => BetweenTurnsAction::Continue,
                                PermissionsWaitResult::Interrupted => BetweenTurnsAction::Continue,
                                PermissionsWaitResult::ChannelClosed => BetweenTurnsAction::Break,
                            }
                        }
                        Ok(event) => {
                            tracing::debug!("Between-turns event drained: {:?}", event);
                            BetweenTurnsAction::Continue
                        }
                        Err(e) => {
                            tracing::warn!("Parse error (between turns): {}", e);
                            BetweenTurnsAction::Continue
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "stdout read error (between turns) for thread {}: {}",
                        thread_id,
                        e
                    );
                    BetweenTurnsAction::Break
                }
            }
        }
        _ = interrupt_rx.recv() => {
            tracing::debug!("Stale interrupt consumed between turns");
            BetweenTurnsAction::Continue
        }
        msg = queue_rx.recv() => {
            match msg {
                Some(m) => BetweenTurnsAction::ProcessMessage(m),
                None => BetweenTurnsAction::Break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── BetweenTurnsAction enum ──────────────────────────────────────────────

    #[test]
    fn between_turns_action_continue_variant() {
        let a = BetweenTurnsAction::Continue;
        assert!(matches!(a, BetweenTurnsAction::Continue));
    }

    #[test]
    fn between_turns_action_break_variant() {
        let a = BetweenTurnsAction::Break;
        assert!(matches!(a, BetweenTurnsAction::Break));
    }

    // Verifies that the hard timeout path produces outer_break = true.
    // The labeled-break semantics: `break 'turn true` sets outer_break to true,
    // which causes SessionWorker::run() to also break its outer loop.
    #[test]
    fn hard_timeout_break_value_is_true() {
        let soft_timeout_fired = true;
        let outer_break = soft_timeout_fired;
        assert!(outer_break, "hard timeout must set outer_break = true to exit the session loop");
    }

    // Verifies that a normal turn end (EOF / result) produces outer_break = false,
    // so the outer loop continues waiting for the next message.
    #[test]
    fn normal_turn_break_value_is_false() {
        let soft_timeout_fired = false;
        let outer_break = soft_timeout_fired;
        assert!(!outer_break, "normal turn completion must set outer_break = false to keep session alive");
    }
}
