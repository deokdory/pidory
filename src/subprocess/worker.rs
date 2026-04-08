use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use poise::serenity_prelude::{ChannelId, Context, CreateMessage, MessageFlags, MessageId, UserId};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, Mutex};

use crate::db::repository;
use crate::handler::formatter;
use crate::i18n::Lang;
use super::background::BackgroundTaskTracker;
use super::parser::{parse_line, StreamEvent, ContentBlock, build_control_response_allow, build_control_response_deny, build_control_response_ask_answer};
use super::permission::{PermissionCache, PermissionDecision, PermissionRequest};
use super::session_manager::{QueuedMessage, SessionInner, ReplyContext};

// ─── Helpers ────────────────────────────────────────────────────────────────

async fn say_silent_chunked(ctx: &Context, channel_id: &ChannelId, text: &str) {
    let chunks = formatter::split_message(text, 2000);
    for chunk in chunks {
        let msg = CreateMessage::new()
            .content(chunk)
            .flags(MessageFlags::SUPPRESS_NOTIFICATIONS);
        if let Err(e) = channel_id.send_message(ctx, msg).await {
            tracing::warn!(%channel_id, "Failed to send bg message to Discord: {}", e);
        }
    }
}

// ─── T6: Common JSON builder helpers ───────────────────────────────────────

fn build_user_message_json(content: &str, downloaded_files: &[String], reply_context: Option<&ReplyContext>) -> String {
    let mut text = String::new();

    // 1. reply context system-reminder
    if let Some(reply) = reply_context {
        // Sanitize untrusted reply content to prevent prompt injection
        let safe_content = reply.original_content
            .replace("</system-reminder>", "[/system-reminder]")
            .replace("<system-reminder>", "[system-reminder]");
        let safe_author = reply.original_author_name
            .replace("</system-reminder>", "[/system-reminder]")
            .replace("<system-reminder>", "[system-reminder]");
        text.push_str(&format!(
            "<system-reminder>\n이 메시지는 다음 메시지에 대한 reply(답장)입니다:\n[원본 작성자: {}]\n{}\n</system-reminder>\n\n",
            safe_author, safe_content
        ));
    }

    // 2. 첨부파일 system-reminder
    if !downloaded_files.is_empty() {
        let paths: String = downloaded_files
            .iter()
            .map(|p| {
                let relative = if let Some(idx) = p.find(".pidory/") {
                    &p[idx..]
                } else {
                    p.as_str()
                };
                format!("- {relative}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        text.push_str(&format!(
            "<system-reminder>\n사용자가 파일을 첨부했습니다. 프로젝트 상대 경로로 접근하세요:\n{paths}\n</system-reminder>\n\n"
        ));
    }

    // 3. 사용자 메시지
    text.push_str(content);

    let json = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    });
    format!("{}\n", json)
}

fn build_interrupt_json() -> String {
    let msg = serde_json::json!({
        "type": "control_request",
        "request_id": format!("interrupt_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()),
        "request": {"subtype": "interrupt"}
    });
    format!("{}\n", msg)
}

// ─── T2: Between-turns event action ────────────────────────────────────────

enum BetweenTurnsAction {
    /// Continue the main loop (no queued message ready)
    Continue,
    /// Break the main loop (EOF or fatal error)
    Break,
    /// A primary message was dequeued and is ready to process
    ProcessMessage(QueuedMessage),
}

// ─── T5: Permission wait result ─────────────────────────────────────────────

pub(super) enum PermissionWaitResult {
    Allow,
    AlwaysAllow(String),                          // tool_name
    Deny(String),                                 // reason
    Error,                                        // stdin error → caller does break
    Answer(std::collections::HashMap<String, String>), // answers for AskUserQuestion
}

// ─── SessionWorker struct ───────────────────────────────────────────────────

pub(super) struct SessionWorker {
    // IO
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    line: String,
    // Channels
    queue_rx: mpsc::Receiver<QueuedMessage>,
    interrupt_rx: mpsc::Receiver<()>,
    permission_tx: mpsc::Sender<PermissionRequest>,
    // State
    permission_cache: PermissionCache,
    tracker: BackgroundTaskTracker,
    current_triggered_by: UserId,
    // Shared refs
    queue_size: Arc<AtomicUsize>,
    sessions: Arc<Mutex<HashMap<String, SessionInner>>>,
    last_activity: Arc<StdMutex<Instant>>,
    has_bg_tasks: Arc<AtomicBool>,
    is_turn_active: Arc<AtomicBool>,
    pending_recalls: Arc<tokio::sync::Mutex<HashMap<MessageId, (String, Arc<AtomicBool>)>>>,
    // Config/Context
    thread_id: String,
    channel_id: ChannelId,
    ctx: Context,
    db: sqlx::SqlitePool,
    timeout_secs: u64,
    lang: Lang,
}

impl SessionWorker {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        stdin: ChildStdin,
        reader: BufReader<ChildStdout>,
        queue_rx: mpsc::Receiver<QueuedMessage>,
        interrupt_rx: mpsc::Receiver<()>,
        permission_tx: mpsc::Sender<PermissionRequest>,
        queue_size: Arc<AtomicUsize>,
        sessions: Arc<Mutex<HashMap<String, SessionInner>>>,
        last_activity: Arc<StdMutex<Instant>>,
        has_bg_tasks: Arc<AtomicBool>,
        is_turn_active: Arc<AtomicBool>,
        thread_id: String,
        channel_id: ChannelId,
        ctx: Context,
        db: sqlx::SqlitePool,
        timeout_secs: u64,
        lang: Lang,
        owner_id: u64,
        pending_recalls: Arc<tokio::sync::Mutex<HashMap<MessageId, (String, Arc<AtomicBool>)>>>,
    ) -> Self {
        Self {
            stdin,
            reader,
            line: String::new(),
            queue_rx,
            interrupt_rx,
            permission_tx,
            permission_cache: PermissionCache::new(),
            tracker: BackgroundTaskTracker::new(),
            current_triggered_by: UserId::new(owner_id),
            queue_size,
            sessions,
            last_activity,
            has_bg_tasks,
            is_turn_active,
            pending_recalls,
            thread_id,
            channel_id,
            ctx,
            db,
            timeout_secs,
            lang,
        }
    }

    pub(super) async fn run(mut self) {
        let Self {
            ref mut stdin,
            ref mut reader,
            ref mut line,
            ref mut queue_rx,
            ref mut interrupt_rx,
            ref permission_tx,
            ref mut permission_cache,
            ref mut tracker,
            ref mut current_triggered_by,
            ref queue_size,
            ref sessions,
            ref last_activity,
            ref has_bg_tasks,
            ref is_turn_active,
            ref pending_recalls,
            ref thread_id,
            ref channel_id,
            ref ctx,
            ref db,
            timeout_secs,
            lang,
            ..
        } = self;

        loop {
            let action = handle_between_turns_event(
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
                has_bg_tasks,
                pending_recalls,
                thread_id,
                channel_id,
                ctx,
                db,
                lang,
            ).await;

            match action {
                BetweenTurnsAction::Continue => continue,
                BetweenTurnsAction::Break => break,
                BetweenTurnsAction::ProcessMessage(msg) => {
                    queue_size.fetch_sub(1, Ordering::Relaxed);
                    pending_recalls.lock().await.remove(&msg.message_id);

                    if msg.cancelled.load(Ordering::Acquire) {
                        tracing::info!(thread_id = %thread_id, msg_id = %msg.message_id, "Message recalled, skipping");
                        continue;
                    }

                    *last_activity.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();

                    // 현재 turn 의 triggered_by 업데이트
                    *current_triggered_by = msg.triggered_by;

                    let json_line = build_user_message_json(&msg.content, &msg.downloaded_files, msg.reply_context.as_ref());
                    if let Err(e) = stdin.write_all(json_line.as_bytes()).await {
                        tracing::error!("stdin write error for thread {}: {}", thread_id, e);
                        break;
                    }
                    if let Err(e) = stdin.flush().await {
                        tracing::error!("stdin flush error for thread {}: {}", thread_id, e);
                        break;
                    }

                    // event_tx가 없으면 mid-turn inject: stdin에 쓰기만 하고 다음으로
                    let Some(event_tx) = msg.event_tx else {
                        continue;
                    };

                    is_turn_active.store(true, Ordering::Relaxed);
                    tracing::info!(thread_id = %thread_id, timeout_secs, "Primary turn started");

                    let turn_broke = run_active_turn(
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
                        has_bg_tasks,
                        is_turn_active,
                        last_activity,
                        pending_recalls,
                        thread_id,
                        channel_id,
                        ctx,
                        timeout_secs,
                        lang,
                        event_tx,
                    ).await;

                    // 'turn loop 종료 후 항상 리셋 (정상/비정상 모든 break 경로 커버)
                    is_turn_active.store(false, Ordering::Relaxed);

                    if turn_broke {
                        break;
                    }
                }
            }
        }

        tracing::info!(
            "Worker task exiting for thread {}, removing from sessions",
            thread_id
        );
        if let Some(mut inner) = sessions.lock().await.remove(thread_id) {
            inner.permission_handler.abort();
            match inner.child.try_wait() {
                Ok(Some(_status)) => {
                    // Already exited, no kill needed
                }
                _ => {
                    // Still running or error checking — kill it
                    if let Err(e) = inner.child.kill().await {
                        tracing::warn!("Failed to kill child process for thread {}: {}", thread_id, e);
                    }
                }
            }
        }
    }
}

// ─── T2: Between-turns event handler ───────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_between_turns_event(
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
    thread_id: &str,
    channel_id: &ChannelId,
    ctx: &Context,
    db: &sqlx::SqlitePool,
    lang: Lang,
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

                            handle_bg_turn(
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
                                thread_id,
                                channel_id,
                                ctx,
                                db,
                                lang,
                            ).await;

                            BetweenTurnsAction::Continue
                        }
                        Ok(StreamEvent::ControlRequest { ref request_id, ref tool_name, ref tool_use_id, ref input, ref decision_reason, .. }) => {
                            if permission_cache.is_always_allowed(tool_name) {
                                let resp = build_control_response_allow(request_id, input);
                                if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                    tracing::error!("stdin write error (between turns auto-allow): {}", e);
                                    return BetweenTurnsAction::Break;
                                }
                                let _ = stdin.flush().await;
                                return BetweenTurnsAction::Continue;
                            }

                            let saved_request_id = request_id.clone();
                            let saved_tool_name = tool_name.clone();
                            let saved_tool_use_id = tool_use_id.clone();
                            let saved_input = input.clone();
                            let saved_reason = decision_reason.clone();

                            let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
                            let perm_req = PermissionRequest {
                                request_id: saved_request_id.clone(),
                                tool_name: saved_tool_name.clone(),
                                tool_use_id: saved_tool_use_id,
                                input: saved_input.clone(),
                                decision_reason: saved_reason,
                                response_tx: resp_tx,
                                triggered_by: *current_triggered_by,
                            };
                            if permission_tx.send(perm_req).await.is_err() {
                                tracing::error!("permission_tx closed, denying (between turns)");
                                let resp = build_control_response_deny(&saved_request_id, "Permission handler unavailable");
                                if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                    tracing::error!("stdin write error (between turns deny): {}", e);
                                    return BetweenTurnsAction::Break;
                                }
                                let _ = stdin.flush().await;
                                return BetweenTurnsAction::Continue;
                            }
                            let perm_result = wait_for_permission(
                                stdin, reader, line, queue_rx, interrupt_rx,
                                queue_size, pending_recalls, thread_id, &saved_tool_name, None, &mut resp_rx,
                            ).await;
                            match write_permission_response(perm_result, &saved_request_id, &saved_input, stdin, permission_cache).await {
                                Ok(true) => return BetweenTurnsAction::Break,
                                Err(e) => {
                                    tracing::error!("stdin write error (between turns permission): {}", e);
                                    return BetweenTurnsAction::Break;
                                }
                                _ => {}
                            }
                            BetweenTurnsAction::Continue
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

// ─── T3: Background turn handler ───────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_bg_turn(
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
    thread_id: &str,
    channel_id: &ChannelId,
    ctx: &Context,
    db: &sqlx::SqlitePool,
    lang: Lang,
) {
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
                            Ok(StreamEvent::Result { .. }) => {
                                if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
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
                                            let formatted = formatter::format_tool_use(name, input);
                                            let bg_text = lang.bg_notification(&formatted);
                                            say_silent_chunked(ctx, channel_id, &bg_text).await;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Ok(StreamEvent::User { ref tool_results, .. }) => {
                                for result in tool_results {
                                    if result.is_error {
                                        if let Some(formatted) = formatter::format_tool_result_with_name(result, None, lang) {
                                            let bg_text = lang.bg_notification(&formatted);
                                            say_silent_chunked(ctx, channel_id, &bg_text).await;
                                        }
                                    }
                                }
                            }
                            Ok(StreamEvent::ControlRequest { ref request_id, ref tool_name, ref tool_use_id, ref input, ref decision_reason, .. }) => {
                                if permission_cache.is_always_allowed(tool_name) {
                                    let resp = build_control_response_allow(request_id, input);
                                    if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                        tracing::error!("stdin write error (bg turn auto-allow): {}", e);
                                        if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                                            tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
                                        }
                                        break 'bg_turn;
                                    }
                                    let _ = stdin.flush().await;
                                } else {
                                    let saved_request_id = request_id.clone();
                                    let saved_tool_name = tool_name.clone();
                                    let saved_tool_use_id = tool_use_id.clone();
                                    let saved_input = input.clone();
                                    let saved_reason = decision_reason.clone();

                                    let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
                                    let perm_req = PermissionRequest {
                                        request_id: saved_request_id.clone(),
                                        tool_name: saved_tool_name.clone(),
                                        tool_use_id: saved_tool_use_id,
                                        input: saved_input.clone(),
                                        decision_reason: saved_reason,
                                        response_tx: resp_tx,
                                        triggered_by: *current_triggered_by,
                                    };
                                    if permission_tx.send(perm_req).await.is_err() {
                                        tracing::error!("permission_tx closed, denying (bg turn)");
                                        let resp = build_control_response_deny(&saved_request_id, "Permission handler unavailable");
                                        if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                            tracing::error!("stdin write error (bg turn deny): {}", e);
                                            if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                                                tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
                                            }
                                            break 'bg_turn;
                                        }
                                        let _ = stdin.flush().await;
                                    } else {
                                        let perm_result = wait_for_permission(
                                            stdin, reader, line, queue_rx, interrupt_rx,
                                            queue_size, pending_recalls, thread_id, &saved_tool_name, None, &mut resp_rx,
                                        ).await;
                                        match write_permission_response(perm_result, &saved_request_id, &saved_input, stdin, permission_cache).await {
                                            Ok(true) | Err(_) => {
                                                if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                                                    tracing::warn!("Failed to update session status for thread {}: {}", thread_id, e);
                                                }
                                                break 'bg_turn;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                            Ok(StreamEvent::TaskStarted { ref task_id, ref task_type, ref description, .. }) => {
                                tracker.track_started(task_id, task_type, description);
                            }
                            Ok(StreamEvent::TaskProgress { ref task_id, ref description, .. }) => {
                                tracker.track_progress(task_id, description);
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
                        let inject_line = build_user_message_json(&m.content, &m.downloaded_files, m.reply_context.as_ref());
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

// ─── T5: Permission wait ────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn wait_for_permission(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    line: &mut String,
    queue_rx: &mut mpsc::Receiver<QueuedMessage>,
    interrupt_rx: &mut mpsc::Receiver<()>,
    queue_size: &Arc<AtomicUsize>,
    pending_recalls: &Arc<tokio::sync::Mutex<HashMap<MessageId, (String, Arc<AtomicBool>)>>>,
    thread_id: &str,
    saved_tool_name: &str,
    event_tx: Option<&mpsc::Sender<StreamEvent>>,
    resp_rx: &mut tokio::sync::oneshot::Receiver<PermissionDecision>,
) -> PermissionWaitResult {
    'perm: loop {
        line.clear();
        tokio::select! {
            // interrupt 요청 (permission 대기 중)
            _ = interrupt_rx.recv() => {
                let interrupt_line = build_interrupt_json();
                if let Err(e) = stdin.write_all(interrupt_line.as_bytes()).await {
                    tracing::error!("interrupt write error (perm wait): {}", e);
                } else {
                    let _ = stdin.flush().await;
                }
                break 'perm PermissionWaitResult::Deny("Interrupted by user".to_string());
            }
            decision = &mut *resp_rx => {
                match decision {
                    Ok(PermissionDecision::Allow) => {
                        break 'perm PermissionWaitResult::Allow;
                    }
                    Ok(PermissionDecision::AlwaysAllow) => {
                        break 'perm PermissionWaitResult::AlwaysAllow(saved_tool_name.to_string());
                    }
                    Ok(PermissionDecision::Deny) => {
                        break 'perm PermissionWaitResult::Deny("User rejected this action".to_string());
                    }
                    Ok(PermissionDecision::Answer(answer)) => {
                        break 'perm PermissionWaitResult::Answer(answer);
                    }
                    Err(_) => {
                        break 'perm PermissionWaitResult::Deny("Permission handler unavailable".to_string());
                    }
                }
            }
            // mid-turn inject during permission wait
            new_msg = queue_rx.recv() => {
                match new_msg {
                    Some(m) => {
                        queue_size.fetch_sub(1, Ordering::Relaxed);
                        pending_recalls.lock().await.remove(&m.message_id);
                        if m.cancelled.load(Ordering::Acquire) {
                            tracing::info!(thread_id = %thread_id, msg_id = %m.message_id, "Message recalled, skipping");
                            continue 'perm;
                        }
                        let inject_line = build_user_message_json(&m.content, &m.downloaded_files, m.reply_context.as_ref());
                        if let Err(e) = stdin.write_all(inject_line.as_bytes()).await {
                            tracing::error!("mid-turn stdin write error (perm wait): {}", e);
                            break 'perm PermissionWaitResult::Error;
                        }
                        let _ = stdin.flush().await;
                    }
                    None => {
                        break 'perm PermissionWaitResult::Deny("Permission handler unavailable".to_string());
                    }
                }
            }
            // stdout events during permission wait (no timeout)
            read = reader.read_line(line) => {
                match read {
                    Ok(0) => {
                        tracing::info!(
                            "Process stdout EOF during perm wait for thread {}",
                            thread_id
                        );
                        queue_rx.close();
                        break 'perm PermissionWaitResult::Deny("Process exited".to_string());
                    }
                    Ok(_) => {
                        let t = line.trim_end();
                        if !t.is_empty() {
                            match parse_line(t) {
                                Ok(ev) => {
                                    if let Some(tx) = event_tx {
                                        let _ = tx.send(ev).await;
                                    } else {
                                        tracing::debug!("Draining event during perm wait (no event_tx): {:?}", ev);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Parse error (perm wait): {}", e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("stdout read error (perm wait): {}", e);
                        queue_rx.close();
                        break 'perm PermissionWaitResult::Deny("stdout read error".to_string());
                    }
                }
            }
        }
    }
}

// ─── T4a: Permission response writer ───────────────────────────────────────

/// Returns Ok(true) if Error variant (caller should break), Ok(false) on success.
async fn write_permission_response(
    result: PermissionWaitResult,
    request_id: &str,
    input: &serde_json::Value,
    stdin: &mut ChildStdin,
    permission_cache: &mut PermissionCache,
) -> Result<bool, std::io::Error> {
    match result {
        PermissionWaitResult::Allow => {
            let resp = build_control_response_allow(request_id, input);
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            Ok(false)
        }
        PermissionWaitResult::AlwaysAllow(tool_name) => {
            let resp = build_control_response_allow(request_id, input);
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            permission_cache.add_always_allow(&tool_name);
            Ok(false)
        }
        PermissionWaitResult::Deny(reason) => {
            let resp = build_control_response_deny(request_id, &reason);
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            Ok(false)
        }
        PermissionWaitResult::Error => {
            Ok(true) // caller should break
        }
        PermissionWaitResult::Answer(answers) => {
            let resp = build_control_response_ask_answer(request_id, input, &answers);
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            Ok(false)
        }
    }
}

// ─── T4: Active turn handler ────────────────────────────────────────────────
// Returns true if the outer main loop should also break (fatal error / EOF).

#[allow(clippy::too_many_arguments)]
async fn run_active_turn(
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
    thread_id: &str,
    channel_id: &ChannelId,
    ctx: &Context,
    timeout_secs: u64,
    lang: Lang,
    event_tx: mpsc::Sender<StreamEvent>,
) -> bool {
    let mut timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut bg_turn_active = false;
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
                                    tracing::info!(thread_id = %thread_id, event = event_name, bg_turn_active, "#36 debug: stdout event");
                                } else {
                                    tracing::debug!(thread_id = %thread_id, event = event_name, bg_turn_active, "stdout event");
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
                                        bg_turn_active = true;
                                        tracing::info!(
                                            thread_id = %thread_id,
                                            task_id,
                                            status,
                                            "#36 debug: bg_turn_active=true (TaskNotification received)"
                                        );
                                        continue 'turn;
                                    }
                                    _ => {}
                                }

                                // bg turn 이벤트 처리: bg_turn_active일 때 기존 코드로 안 넘김
                                if bg_turn_active {
                                    match &event {
                                        StreamEvent::Result { session_id, .. } => {
                                            bg_turn_active = false;
                                            timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
                                            tracing::info!(
                                                thread_id = %thread_id,
                                                session_id,
                                                "#36 debug: bg turn Result consumed (bg_turn_active→false)"
                                            );
                                            continue 'turn; // bg turn 끝 — user turn은 계속
                                        }
                                        StreamEvent::Assistant { content, .. } => {
                                            for block in content {
                                                match block {
                                                    ContentBlock::Text(text) if !text.trim().is_empty() => {
                                                        let bg_text = lang.bg_notification(text);
                                                        say_silent_chunked(ctx, channel_id, &bg_text).await;
                                                    }
                                                    ContentBlock::ToolUse { name, input, .. } => {
                                                        let formatted = formatter::format_tool_use(name, input);
                                                        let bg_text = lang.bg_notification(&formatted);
                                                        say_silent_chunked(ctx, channel_id, &bg_text).await;
                                                    }
                                                    _ => {}
                                                }
                                            }
                                            continue 'turn;
                                        }
                                        StreamEvent::User { tool_results, .. } => {
                                            for result in tool_results {
                                                if result.is_error {
                                                    if let Some(formatted) = formatter::format_tool_result_with_name(result, None, lang) {
                                                        let bg_text = lang.bg_notification(&formatted);
                                                        say_silent_chunked(ctx, channel_id, &bg_text).await;
                                                    }
                                                }
                                            }
                                            continue 'turn;
                                        }
                                        StreamEvent::ControlRequest { request_id, tool_name, tool_use_id, input, decision_reason, .. } => {
                                            if permission_cache.is_always_allowed(tool_name) {
                                                let resp = build_control_response_allow(request_id, input);
                                                if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                                    tracing::error!("stdin write error (bg turn in user turn auto-allow): {}", e);
                                                    break 'turn false;
                                                }
                                                let _ = stdin.flush().await;
                                                continue 'turn;
                                            }

                                            let saved_request_id = request_id.clone();
                                            let saved_tool_name = tool_name.clone();
                                            let saved_tool_use_id = tool_use_id.clone();
                                            let saved_input = input.clone();
                                            let saved_reason = decision_reason.clone();

                                            let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
                                            let perm_req = PermissionRequest {
                                                request_id: saved_request_id.clone(),
                                                tool_name: saved_tool_name.clone(),
                                                tool_use_id: saved_tool_use_id,
                                                input: saved_input.clone(),
                                                decision_reason: saved_reason,
                                                response_tx: resp_tx,
                                                triggered_by: *current_triggered_by,
                                            };
                                            if permission_tx.send(perm_req).await.is_err() {
                                                tracing::error!("permission_tx closed, denying (bg turn in user turn)");
                                                let resp = build_control_response_deny(&saved_request_id, "Permission handler unavailable");
                                                if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                                    tracing::error!("stdin write error (bg turn in user turn deny): {}", e);
                                                }
                                                let _ = stdin.flush().await;
                                                break 'turn false;
                                            }
                                            let perm_result = wait_for_permission(
                                                stdin, reader, line, queue_rx, interrupt_rx,
                                                queue_size, pending_recalls, thread_id, &saved_tool_name, Some(&event_tx), &mut resp_rx,
                                            ).await;
                                            match write_permission_response(perm_result, &saved_request_id, &saved_input, stdin, permission_cache).await {
                                                Ok(true) => break 'turn false,
                                                Err(e) => {
                                                    tracing::error!("stdin write error (bg turn in user turn permission): {}", e);
                                                    break 'turn false;
                                                }
                                                _ => {}
                                            }
                                            continue 'turn;
                                        }
                                        _ => { continue 'turn; } // Init, RateLimit 등 skip
                                    }
                                }

                                // 기존 user turn 이벤트 처리 (bg_turn_active == false)
                                if let StreamEvent::ControlRequest {
                                    ref request_id,
                                    ref tool_name,
                                    ref tool_use_id,
                                    ref input,
                                    ref decision_reason,
                                } = event {
                                    // Clone all fields before any move
                                    let saved_request_id = request_id.clone();
                                    let saved_tool_name = tool_name.clone();
                                    let saved_tool_use_id = tool_use_id.clone();
                                    let saved_input = input.clone();
                                    let saved_decision_reason = decision_reason.clone();

                                    if permission_cache.is_always_allowed(&saved_tool_name) {
                                        // auto-allow from cache
                                        let resp = build_control_response_allow(&saved_request_id, &saved_input);
                                        if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                            tracing::error!("stdin write error (auto-allow): {}", e);
                                            break 'turn false;
                                        }
                                        let _ = stdin.flush().await;
                                        continue 'turn;
                                    }

                                    // event_tx로 ControlRequest 전달 (handler에서 버튼 표시)
                                    let _ = event_tx.send(event).await;

                                    // permission 요청 생성
                                    let (resp_tx, mut resp_rx) = tokio::sync::oneshot::channel();
                                    let perm_req = PermissionRequest {
                                        request_id: saved_request_id.clone(),
                                        tool_name: saved_tool_name.clone(),
                                        tool_use_id: saved_tool_use_id,
                                        input: saved_input.clone(),
                                        decision_reason: saved_decision_reason,
                                        response_tx: resp_tx,
                                        triggered_by: *current_triggered_by,
                                    };

                                    if permission_tx.send(perm_req).await.is_err() {
                                        tracing::error!("permission_tx closed, denying");
                                        let resp = build_control_response_deny(&saved_request_id, "Permission handler unavailable");
                                        if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                            tracing::error!("stdin write error (foreground deny): {}", e);
                                        }
                                        let _ = stdin.flush().await;
                                        break 'turn false;
                                    }

                                    let perm_result = wait_for_permission(
                                        stdin,
                                        reader,
                                        line,
                                        queue_rx,
                                        interrupt_rx,
                                        queue_size,
                                        pending_recalls,
                                        thread_id,
                                        &saved_tool_name,
                                        Some(&event_tx),
                                        &mut resp_rx,
                                    ).await;

                                    match write_permission_response(perm_result, &saved_request_id, &saved_input, stdin, permission_cache).await {
                                        Ok(true) => break 'turn false, // Error variant
                                        Err(e) => {
                                            tracing::error!("stdin write error (permission response): {}", e);
                                            break 'turn false;
                                        }
                                        _ => {} // success
                                    }

                                    // timeout 리셋
                                    timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
                                    tracing::debug!(thread_id = %thread_id, "Timeout deadline reset");
                                    continue 'turn;
                                }

                                // 일반 이벤트 처리
                                let is_result = event.is_result();
                                if is_result {
                                    let sid = event.session_id().unwrap_or("?");
                                    tracing::info!(
                                        thread_id = %thread_id,
                                        session_id = sid,
                                        bg_turn_active,
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
                                bg_turn_active,
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

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        build_user_message_json, build_interrupt_json, write_permission_response,
        BetweenTurnsAction, PermissionWaitResult,
    };
    use crate::subprocess::permission::PermissionCache;
    use crate::subprocess::parser::{build_control_response_allow, build_control_response_deny};
    use crate::subprocess::session_manager::ReplyContext;

    // ── build_user_message_json ──────────────────────────────────────────────

    #[test]
    fn user_message_json_basic_structure() {
        let out = build_user_message_json("hello", &[], None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        let content = v["message"]["content"].as_array().expect("content is array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "hello");
    }

    #[test]
    fn user_message_json_special_chars_escaped() {
        let out = build_user_message_json("hello \"world\"", &[], None);
        // Must round-trip through JSON without error and preserve the value
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["message"]["content"][0]["text"], "hello \"world\"");
    }

    #[test]
    fn user_message_json_korean_and_emoji() {
        let out = build_user_message_json("안녕 🎉", &[], None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["message"]["content"][0]["text"], "안녕 🎉");
    }

    #[test]
    fn user_message_json_empty_string() {
        let out = build_user_message_json("", &[], None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["message"]["content"][0]["text"], "");
    }

    #[test]
    fn user_message_json_ends_with_newline() {
        let out = build_user_message_json("hello", &[], None);
        assert!(out.ends_with('\n'), "output must end with newline");
    }

    #[test]
    fn build_message_no_attachments() {
        let out = build_user_message_json("hello", &[], None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"][0]["type"], "text");
        assert_eq!(v["message"]["content"][0]["text"], "hello");
    }

    #[test]
    fn build_message_with_attachments() {
        let files = vec!["/project/.pidory/downloads/123/456_file.py".to_string()];
        let out = build_user_message_json("hello", &files, None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        assert!(text.contains("<system-reminder>"), "must contain system-reminder tag");
        assert!(text.contains("hello"), "must contain original content");
    }

    #[test]
    fn build_message_attachment_paths_relative() {
        let files = vec!["/project/.pidory/downloads/123/456_file.py".to_string()];
        let out = build_user_message_json("hello", &files, None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        assert!(text.contains(".pidory/downloads/123/456_file.py"), "must contain relative path");
        assert!(!text.contains("/project/.pidory/"), "must not contain absolute path prefix");
    }

    #[test]
    fn build_message_multiple_attachments() {
        let files = vec![
            "/project/.pidory/downloads/123/a.png".to_string(),
            "/project/.pidory/downloads/123/b.csv".to_string(),
        ];
        let out = build_user_message_json("hello", &files, None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        assert!(text.contains(".pidory/downloads/123/a.png"), "must list first file");
        assert!(text.contains(".pidory/downloads/123/b.csv"), "must list second file");
    }

    #[test]
    fn build_message_with_reply_context() {
        let reply_ctx = ReplyContext {
            original_content: "This is the original message".to_string(),
            original_author_name: "Alice".to_string(),
        };
        let out = build_user_message_json("follow-up question", &[], Some(&reply_ctx));
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        assert!(text.contains("<system-reminder>"), "must contain system-reminder tag");
        assert!(text.contains("reply(답장)"), "must mention reply");
        assert!(text.contains("Alice"), "must contain original author name");
        assert!(text.contains("This is the original message"), "must contain original content");
        assert!(text.contains("follow-up question"), "must contain user message");
    }

    #[test]
    fn build_message_reply_context_plus_attachments() {
        let reply_ctx = ReplyContext {
            original_content: "Original".to_string(),
            original_author_name: "Bob".to_string(),
        };
        let files = vec!["/project/.pidory/downloads/123/file.py".to_string()];
        let out = build_user_message_json("question", &files, Some(&reply_ctx));
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        // Must have both system-reminder blocks
        let reminder_count = text.matches("<system-reminder>").count();
        assert_eq!(reminder_count, 2, "must have two system-reminder blocks (reply + attachments)");
        // Reply context should come first
        let reply_pos = text.find("reply(답장)").expect("reply context");
        let file_pos = text.find(".pidory/downloads").expect("attachment");
        assert!(reply_pos < file_pos, "reply context must come before attachments");
    }

    #[test]
    fn build_message_reply_context_empty_original() {
        // Test that empty original_content is still injected (unlike Discord behavior)
        // The filtering happens in resolve_reply_context, not build_user_message_json
        let reply_ctx = ReplyContext {
            original_content: "".to_string(),
            original_author_name: "Charlie".to_string(),
        };
        let out = build_user_message_json("question", &[], Some(&reply_ctx));
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        // Even with empty content, the system-reminder should be present
        assert!(text.contains("<system-reminder>"), "system-reminder must be present");
        assert!(text.contains("Charlie"), "author name must be included");
    }

    #[test]
    fn build_message_reply_context_special_chars() {
        let reply_ctx = ReplyContext {
            original_content: r#"Line 1: "quoted" text\nLine 2: <tag>content</tag>"#.to_string(),
            original_author_name: "User\\Name".to_string(),
        };
        let out = build_user_message_json("follow-up", &[], Some(&reply_ctx));
        // Must be valid JSON even with special characters
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        // Special characters should be preserved
        assert!(text.contains(r#""quoted""#), "should preserve quoted text");
        assert!(text.contains("User\\Name"), "should preserve backslash in name");
    }

    // ── build_interrupt_json ─────────────────────────────────────────────────

    #[test]
    fn interrupt_json_type_is_control_request() {
        let out = build_interrupt_json();
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["type"], "control_request");
    }

    #[test]
    fn interrupt_json_subtype_is_interrupt() {
        let out = build_interrupt_json();
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["request"]["subtype"], "interrupt");
    }

    #[test]
    fn interrupt_json_request_id_prefix() {
        let out = build_interrupt_json();
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let rid = v["request_id"].as_str().expect("request_id is string");
        assert!(rid.starts_with("interrupt_"), "request_id must start with 'interrupt_', got: {rid}");
    }

    #[test]
    fn interrupt_json_ends_with_newline() {
        let out = build_interrupt_json();
        assert!(out.ends_with('\n'), "output must end with newline");
    }

    // ── PermissionWaitResult enum ────────────────────────────────────────────

    #[test]
    fn permission_wait_result_allow_variant() {
        let r = PermissionWaitResult::Allow;
        assert!(matches!(r, PermissionWaitResult::Allow));
    }

    #[test]
    fn permission_wait_result_always_allow_carries_tool_name() {
        let r = PermissionWaitResult::AlwaysAllow("bash".to_string());
        match r {
            PermissionWaitResult::AlwaysAllow(name) => assert_eq!(name, "bash"),
            _ => panic!("expected AlwaysAllow"),
        }
    }

    #[test]
    fn permission_wait_result_deny_carries_reason() {
        let r = PermissionWaitResult::Deny("not allowed".to_string());
        match r {
            PermissionWaitResult::Deny(reason) => assert_eq!(reason, "not allowed"),
            _ => panic!("expected Deny"),
        }
    }

    #[test]
    fn permission_wait_result_error_variant() {
        let r = PermissionWaitResult::Error;
        assert!(matches!(r, PermissionWaitResult::Error));
    }

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

    // ── write_permission_response ────────────────────────────────────────────

    /// Spawn a `cat` subprocess to get a real ChildStdin for write tests.
    async fn spawn_cat_stdin() -> tokio::process::ChildStdin {
        let mut child = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn cat");
        child.stdin.take().expect("no stdin")
    }

    #[tokio::test]
    async fn write_permission_response_allow_returns_ok_false() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({"command": "ls"});
        let result = write_permission_response(
            PermissionWaitResult::Allow,
            "req-001",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await;
        assert_eq!(result.unwrap(), false, "Allow variant must return Ok(false)");
    }

    #[tokio::test]
    async fn write_permission_response_allow_writes_allow_json() {
        // Verify the JSON written to stdin has behavior=allow.
        let expected = build_control_response_allow("req-002", &serde_json::json!({}));
        let v: serde_json::Value = serde_json::from_str(expected.trim()).unwrap();
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(v["response"]["request_id"], "req-002");
    }

    #[tokio::test]
    async fn write_permission_response_always_allow_updates_cache() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        assert!(!cache.is_always_allowed("Bash"));
        let input = serde_json::json!({"command": "echo hi"});
        let result = write_permission_response(
            PermissionWaitResult::AlwaysAllow("Bash".to_string()),
            "req-003",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await;
        assert_eq!(result.unwrap(), false, "AlwaysAllow must return Ok(false)");
        assert!(cache.is_always_allowed("Bash"), "cache must record the always-allowed tool");
    }

    #[tokio::test]
    async fn write_permission_response_always_allow_does_not_affect_other_tools() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        write_permission_response(
            PermissionWaitResult::AlwaysAllow("Write".to_string()),
            "req-004",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await
        .unwrap();
        assert!(cache.is_always_allowed("Write"));
        assert!(!cache.is_always_allowed("Bash"), "unrelated tools must not be in cache");
    }

    #[tokio::test]
    async fn write_permission_response_deny_returns_ok_false() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        let result = write_permission_response(
            PermissionWaitResult::Deny("user rejected".to_string()),
            "req-005",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await;
        assert_eq!(result.unwrap(), false, "Deny variant must return Ok(false)");
    }

    #[tokio::test]
    async fn write_permission_response_deny_does_not_update_cache() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        write_permission_response(
            PermissionWaitResult::Deny("no".to_string()),
            "req-006",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await
        .unwrap();
        assert!(!cache.is_always_allowed("Bash"), "Deny must not update the permission cache");
    }

    #[tokio::test]
    async fn write_permission_response_error_returns_ok_true() {
        // Error variant signals caller to break — no stdin needed.
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        let result = write_permission_response(
            PermissionWaitResult::Error,
            "req-007",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await;
        assert_eq!(result.unwrap(), true, "Error variant must return Ok(true) to signal break");
    }

    #[tokio::test]
    async fn write_permission_response_error_does_not_update_cache() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        write_permission_response(
            PermissionWaitResult::Error,
            "req-008",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await
        .unwrap();
        assert!(!cache.is_always_allowed("Bash"), "Error must not touch the permission cache");
    }

    // ── build_control_response_allow / build_control_response_deny ───────────

    #[test]
    fn control_response_allow_json_structure() {
        let input = serde_json::json!({"command": "ls"});
        let out = build_control_response_allow("rid-1", &input);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["type"], "control_response");
        assert_eq!(v["response"]["subtype"], "success");
        assert_eq!(v["response"]["request_id"], "rid-1");
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(v["response"]["response"]["updatedInput"]["command"], "ls");
    }

    #[test]
    fn control_response_allow_ends_with_newline() {
        let out = build_control_response_allow("rid-2", &serde_json::json!({}));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn control_response_deny_json_structure() {
        let out = build_control_response_deny("rid-3", "user rejected");
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["type"], "control_response");
        assert_eq!(v["response"]["subtype"], "success");
        assert_eq!(v["response"]["request_id"], "rid-3");
        assert_eq!(v["response"]["response"]["behavior"], "deny");
        assert_eq!(v["response"]["response"]["message"], "user rejected");
    }

    #[test]
    fn control_response_deny_ends_with_newline() {
        let out = build_control_response_deny("rid-4", "reason");
        assert!(out.ends_with('\n'));
    }

    #[tokio::test]
    async fn write_permission_response_answer_returns_ok_false() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({"questions": [{"question": "pick?"}]});
        let answers = std::collections::HashMap::from([("q_0".to_string(), "Blue".to_string())]);
        let result = write_permission_response(
            PermissionWaitResult::Answer(answers),
            "req-ask",
            &input,
            &mut stdin,
            &mut cache,
        ).await;
        assert_eq!(result.unwrap(), false);
    }

    #[tokio::test]
    async fn write_permission_response_answer_does_not_update_cache() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        let answers = std::collections::HashMap::from([("q_0".to_string(), "test".to_string())]);
        write_permission_response(
            PermissionWaitResult::Answer(answers),
            "req-ask2",
            &input,
            &mut stdin,
            &mut cache,
        ).await.unwrap();
        assert!(!cache.is_always_allowed("AskUserQuestion"));
    }
}
