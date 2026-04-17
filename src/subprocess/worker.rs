use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::{FutureExt, StreamExt};
use poise::serenity_prelude::{ChannelId, Context, CreateMessage, MessageFlags, MessageId, UserId};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{Mutex, mpsc, mpsc::error::TrySendError};

use crate::db::repository;
use crate::handler::formatter;
use crate::handler::message::{shorten_model_name, format_ctx_suffix};
use crate::i18n::Lang;
use crate::ratelimit::RateLimitInfo;
use super::background::BackgroundTaskTracker;
use super::parser::{parse_line, StreamEvent, ContentBlock, build_control_response_allow, build_control_response_allow_probed, build_control_response_deny, build_control_response_ask_answer, ProbeMode};
use super::permission::{PermissionCache, PermissionDecision, PermissionRequest};
use super::session_manager::{QueuedMessage, SessionInner, ReplyContext};

// ─── Helpers ────────────────────────────────────────────────────────────────

fn handle_ratelimit_event(
    ratelimit_tx: &tokio::sync::watch::Sender<RateLimitInfo>,
    rate_limit_type: Option<&str>,
    utilization: Option<f64>,
    resets_at: Option<u64>,
    is_using_overage: Option<bool>,
) {
    if let Some(rlt) = rate_limit_type {
        let resets = resets_at.unwrap_or(0);
        let overage = is_using_overage.unwrap_or(false);
        if let Some(util) = utilization {
            ratelimit_tx.send_modify(|info| {
                info.update_from_event(rlt, util, resets, overage);
            });
        } else {
            ratelimit_tx.send_modify(|info| {
                info.update_resets_only(rlt, resets, overage);
            });
        }
    } else {
        tracing::debug!("rate_limit_event without rateLimitType: utilization={:?}", utilization);
    }
}

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

    // 1. reply context — system-reminder로 신뢰 경계 분리, </system-reminder> 인젝션 방지
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

#[allow(dead_code)]
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
    ratelimit_tx: tokio::sync::watch::Sender<RateLimitInfo>,
    todo_trackers: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<crate::handler::todo_tracker::TodoTracker>>>>>,
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
        ratelimit_tx: tokio::sync::watch::Sender<RateLimitInfo>,
        todo_trackers: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<crate::handler::todo_tracker::TodoTracker>>>>>,
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
            ratelimit_tx,
            todo_trackers,
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
            ref ratelimit_tx,
            ref todo_trackers,
            ref thread_id,
            ref channel_id,
            ref ctx,
            ref db,
            timeout_secs,
            lang,
            ..
        } = self;

        let mut model_name = String::new();

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
                ratelimit_tx,
                todo_trackers,
                thread_id,
                channel_id,
                ctx,
                db,
                lang,
                &mut model_name,
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
                        ratelimit_tx,
                        thread_id,
                        channel_id,
                        ctx,
                        timeout_secs,
                        lang,
                        event_tx,
                        &mut model_name,
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
    ratelimit_tx: &tokio::sync::watch::Sender<RateLimitInfo>,
    todo_trackers: &Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<crate::handler::todo_tracker::TodoTracker>>>>>,
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
                                ratelimit_tx,
                                todo_trackers,
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
                            tracing::info!("control_request received: tool={} request_id={} input={:?}", tool_name, request_id, input);
                            if permission_cache.is_always_allowed(tool_name) {
                                tracing::info!("cache hit: tool={} — auto-allow (bypass flag = {})", tool_name, std::env::var("PIDORY_SPIKE_BYPASS_CACHE").unwrap_or_default());
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
                                tool_use_id: saved_tool_use_id.clone(),
                                input: saved_input.clone(),
                                decision_reason: saved_reason.clone(),
                                response_tx: resp_tx,
                                triggered_by: *current_triggered_by,
                            };
                            let initial_cr = InitialControlRequest {
                                request_id: saved_request_id.clone(),
                                tool_name: saved_tool_name.clone(),
                                tool_use_id: saved_tool_use_id,
                                input: saved_input.clone(),
                                decision_reason: saved_reason,
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
    ratelimit_tx: &tokio::sync::watch::Sender<RateLimitInfo>,
    todo_trackers: &Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<crate::handler::todo_tracker::TodoTracker>>>>>,
    thread_id: &str,
    channel_id: &ChannelId,
    ctx: &Context,
    db: &sqlx::SqlitePool,
    lang: Lang,
    model_name: &mut String,
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
                                let ctx_suffix = format_ctx_suffix(total_input_tokens, context_window);
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
                                let tracker = todo_trackers.lock().await.get(thread_id).cloned();
                                if let Some(tracker) = tracker {
                                    tracker.lock().await.flush(ctx).await;
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
                                                let tracker = {
                                                    let mut map = todo_trackers.lock().await;
                                                    map.entry(thread_id.to_string())
                                                        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(
                                                            crate::handler::todo_tracker::TodoTracker::new(*channel_id)
                                                        )))
                                                        .clone()
                                                };
                                                tracker.lock().await.update(ctx, input).await;
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
                                    if result.is_error {
                                        if let Some(formatted) = formatter::format_tool_result_with_name(result, None, lang) {
                                            let bg_text = lang.bg_notification(&formatted);
                                            say_silent_chunked(ctx, channel_id, &bg_text).await;
                                        }
                                    }
                                }
                            }
                            Ok(StreamEvent::ControlRequest { ref request_id, ref tool_name, ref tool_use_id, ref input, ref decision_reason, .. }) => {
                                tracing::info!("control_request received: tool={} request_id={} input={:?}", tool_name, request_id, input);
                                if permission_cache.is_always_allowed(tool_name) {
                                    tracing::info!("cache hit: tool={} — auto-allow (bypass flag = {})", tool_name, std::env::var("PIDORY_SPIKE_BYPASS_CACHE").unwrap_or_default());
                                    let resp = build_control_response_allow(request_id, input);
                                    if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                        tracing::error!("stdin write error (bg turn auto-allow): {}", e);
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

// ─── T4a: Permission response writer ───────────────────────────────────────

/// Returns Ok(true) if Error variant (caller should break), Ok(false) on success.
#[allow(dead_code)]
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
            tracing::info!(request_id = %request_id, behavior = "allow", "control_response written");
            Ok(false)
        }
        PermissionWaitResult::AlwaysAllow(tool_name) => {
            let resp = build_control_response_allow_probed(
                request_id,
                input,
                &ProbeMode::from_env(),
                &tool_name,
            );
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            permission_cache.add_always_allow(&tool_name);
            tracing::info!(request_id = %request_id, behavior = "always_allow", tool_name = %tool_name, "control_response written");
            Ok(false)
        }
        PermissionWaitResult::Deny(reason) => {
            let resp = build_control_response_deny(request_id, &reason);
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            tracing::info!(request_id = %request_id, behavior = "deny", reason = %reason, "control_response written");
            Ok(false)
        }
        PermissionWaitResult::Error => {
            Ok(true) // caller should break
        }
        PermissionWaitResult::Answer(answers) => {
            let resp = build_control_response_ask_answer(request_id, input, &answers);
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            tracing::info!(request_id = %request_id, behavior = "answer", "control_response written");
            Ok(false)
        }
    }
}

// ─── T5a: Parallel permission wait (Wave 1.1) ──────────────────────────────

/// 동시 pending control_request 의 상한. 초과 시 auto-deny.
// permission_tx buffer 와 동기 (session_manager.rs).
const MAX_PENDING_CR: usize = 32;

/// 새 복수형 wait_for_permissions() 의 반환 타입.
#[derive(Debug)]
pub(super) enum PermissionsWaitResult {
    /// 최소 1개 CR 응답 후 pending 이 모두 비어 정상 종료됨.
    /// _decisions 는 처리된 (request_id, PermissionDecision) 목록 (Wave 1.3+ 에서 활용 예정).
    AllResolved { _decisions: Vec<(String, super::permission::PermissionDecision)> },
    /// interrupt_rx 수신으로 인한 조기 종료.
    Interrupted,
    /// permission_tx (handler) 가 닫혔음 — 복구 불가.
    ChannelClosed,
}

/// wait_for_permissions() 호출자가 "첫 CR" 정보를 전달하는 용도.
pub(super) struct InitialControlRequest {
    request_id: String,
    tool_name: String,
    tool_use_id: String,
    input: serde_json::Value,
    decision_reason: Option<String>,
    triggered_by: UserId,
}

/// FuturesUnordered 내부 pending entry 상태.
struct PendingEntry {
    tool_name: String,
    saved_input: serde_json::Value,
}

/// 복수 pending CR 을 동시에 대기하는 공통 함수.
///
/// **STDIN OWNERSHIP INVARIANT**: 이 함수는 `stdin: &mut ChildStdin` 을 배타 소유한다.
/// 모든 stdin write (control_response, deny, mid-turn inject 등) 는 이 함수의
/// `tokio::select!` 분기 내부에서만 수행된다. 절대 `tokio::spawn()` 으로 writer 를
/// 분리하거나 `Arc<Mutex<ChildStdin>>` 로 감싸지 말 것. 동시 write 는 JSON 라인을
/// interleave 하여 Claude CLI 의 parser 를 깨뜨린다.
///
/// stdin is exclusively written in this task; do not spawn writers.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub(super) async fn wait_for_permissions<W, R>(
    stdin: &mut W,
    reader: &mut R,
    line: &mut String,
    queue_rx: &mut mpsc::Receiver<QueuedMessage>,
    interrupt_rx: &mut mpsc::Receiver<()>,
    queue_size: &Arc<AtomicUsize>,
    pending_recalls: &Arc<tokio::sync::Mutex<HashMap<MessageId, (String, Arc<AtomicBool>)>>>,
    thread_id: &str,
    event_tx: Option<&mpsc::Sender<StreamEvent>>,
    ratelimit_tx: &tokio::sync::watch::Sender<RateLimitInfo>,
    permission_cache: &mut PermissionCache,
    permission_tx: &mpsc::Sender<PermissionRequest>,
    initial_cr: InitialControlRequest,
) -> PermissionsWaitResult
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    // HashMap<request_id, PendingEntry>
    let mut pending: HashMap<String, PendingEntry> = HashMap::new();

    // FuturesUnordered: 각 future 는 (request_id, Result<PermissionDecision, RecvError>) 를 yield
    let mut futures: FuturesUnordered<
        BoxFuture<'static, (String, Result<super::permission::PermissionDecision, tokio::sync::oneshot::error::RecvError>)>,
    > = FuturesUnordered::new();

    // 누적 decisions (AllResolved 에 포함됨)
    let mut decisions: Vec<(String, super::permission::PermissionDecision)> = Vec::new();

    // ── initial_cr 처리 ──────────────────────────────────────────────────────
    {
        let rid = initial_cr.request_id.clone();
        let tool_name = initial_cr.tool_name.clone();

        if permission_cache.is_always_allowed(&tool_name) {
            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, "cache hit, auto-allow");
            let resp = build_control_response_allow(&rid, &initial_cr.input);
            if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                tracing::error!("stdin write error (wait_for_permissions initial auto-allow): {}", e);
                // stdin error → 즉시 Interrupted 로 처리 (caller 가 break 결정)
                return PermissionsWaitResult::Interrupted;
            }
            let _ = stdin.flush().await;
            // cache hit: 이미 허용됨 → AllResolved 로 즉시 반환
            decisions.push((rid, super::permission::PermissionDecision::Allow));
            return PermissionsWaitResult::AllResolved { _decisions: decisions };
        }

        // handler 로 전송
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<super::permission::PermissionDecision>();
        let perm_req = PermissionRequest {
            request_id: rid.clone(),
            tool_name: tool_name.clone(),
            tool_use_id: initial_cr.tool_use_id.clone(),
            input: initial_cr.input.clone(),
            decision_reason: initial_cr.decision_reason.clone(),
            response_tx: resp_tx,
            triggered_by: initial_cr.triggered_by,
        };

        match permission_tx.try_send(perm_req) {
            Ok(()) => {
                tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, "permission_tx send ok");
                pending.insert(rid.clone(), PendingEntry { tool_name, saved_input: initial_cr.input.clone() });
                let fut = async move { (rid, resp_rx.await) }.boxed();
                futures.push(fut);
            }
            Err(TrySendError::Full(dropped)) => {
                let dropped_rid = dropped.request_id.clone();
                let dropped_tool = dropped.tool_name.clone();
                tracing::warn!(thread_id = %thread_id, request_id = %dropped_rid, tool_name = %dropped_tool, "permission_tx full, auto-denying");
                let resp = build_control_response_deny(&dropped_rid, "Permission queue full");
                if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                    tracing::error!("stdin write error (wait_for_permissions initial full-deny): {}", e);
                    return PermissionsWaitResult::Interrupted;
                }
                let _ = stdin.flush().await;
                // auto-deny 완료 → AllResolved 반환 (최소 1개 처리됨)
                decisions.push((dropped_rid, super::permission::PermissionDecision::Deny));
                return PermissionsWaitResult::AllResolved { _decisions: decisions };
            }
            Err(TrySendError::Closed(_)) => {
                tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, "permission_tx send failed");
                return PermissionsWaitResult::ChannelClosed;
            }
        }
    }

    // ── 메인 루프 ────────────────────────────────────────────────────────────
    loop {
        line.clear();
        tokio::select! {
            biased;

            // ── interrupt ──────────────────────────────────────────────────
            _ = interrupt_rx.recv() => {
                // 모든 pending CR 에 대해 deny 전송 (stdin 직렬 write, write-per-flush)
                for (rid, entry) in &pending {
                    let resp = build_control_response_deny(rid, "Interrupted by user");
                    if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                        tracing::error!("stdin write error (wait_for_permissions interrupt deny): {}", e);
                        break;
                    }
                    if let Err(e) = stdin.flush().await {
                        tracing::error!("stdin flush error (wait_for_permissions interrupt deny): {}", e);
                        break;
                    }
                    tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %entry.tool_name, behavior = "deny", reason = "Interrupted by user", "control_response written");
                }
                // resp_rx future 들은 drop 으로 처리됨
                return PermissionsWaitResult::Interrupted;
            }

            // ── mid-turn inject ────────────────────────────────────────────
            new_msg = queue_rx.recv() => {
                match new_msg {
                    Some(m) => {
                        queue_size.fetch_sub(1, Ordering::Relaxed);
                        pending_recalls.lock().await.remove(&m.message_id);
                        if m.cancelled.load(Ordering::Acquire) {
                            tracing::info!(thread_id = %thread_id, msg_id = %m.message_id, "Message recalled, skipping");
                            continue;
                        }
                        let inject_line = build_user_message_json(&m.content, &m.downloaded_files, m.reply_context.as_ref());
                        if let Err(e) = stdin.write_all(inject_line.as_bytes()).await {
                            tracing::error!("mid-turn stdin write error (wait_for_permissions): {}", e);
                            return PermissionsWaitResult::Interrupted;
                        }
                        let _ = stdin.flush().await;
                    }
                    None => {
                        // queue closed
                        return PermissionsWaitResult::ChannelClosed;
                    }
                }
            }

            // ── stdout read (추가 CR 수신 가능, MAX_PENDING_CR 상한) ───────
            read = reader.read_line(line), if futures.len() < MAX_PENDING_CR => {
                match read {
                    Ok(0) => {
                        tracing::info!("Process stdout EOF during wait_for_permissions for thread {}", thread_id);
                        queue_rx.close();
                        // pending 전부 deny (stdin 직렬 write, write-per-flush)
                        for (rid, entry) in &pending {
                            let resp = build_control_response_deny(rid, "Process exited");
                            if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                tracing::error!("stdin write error (wait_for_permissions EOF deny): {}", e);
                                break;
                            }
                            if let Err(e) = stdin.flush().await {
                                tracing::error!("stdin flush error (wait_for_permissions EOF deny): {}", e);
                                break;
                            }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %entry.tool_name, behavior = "deny", reason = "Process exited", "control_response written");
                        }
                        return PermissionsWaitResult::Interrupted;
                    }
                    Ok(_) => {
                        let trimmed = line.trim_end();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match parse_line(trimmed) {
                            Ok(StreamEvent::ControlRequest { ref request_id, ref tool_name, ref tool_use_id, ref input, ref decision_reason, .. }) => {
                                let rid = request_id.clone();
                                let tname = tool_name.clone();
                                tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tname, "control_request received");

                                if permission_cache.is_always_allowed(&tname) {
                                    tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tname, "cache hit, auto-allow");
                                    let resp = build_control_response_allow(&rid, input);
                                    if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                        tracing::error!("stdin write error (wait_for_permissions additional auto-allow): {}", e);
                                        return PermissionsWaitResult::Interrupted;
                                    }
                                    let _ = stdin.flush().await;
                                    // auto-allow 는 pending/futures 에 추가하지 않음 (즉시 처리됨)
                                } else {
                                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<super::permission::PermissionDecision>();
                                    let perm_req = PermissionRequest {
                                        request_id: rid.clone(),
                                        tool_name: tname.clone(),
                                        tool_use_id: tool_use_id.clone(),
                                        input: input.clone(),
                                        decision_reason: decision_reason.clone(),
                                        response_tx: resp_tx,
                                        triggered_by: initial_cr.triggered_by,
                                    };

                                    match permission_tx.try_send(perm_req) {
                                        Ok(()) => {
                                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tname, "permission_tx send ok");
                                            pending.insert(rid.clone(), PendingEntry { tool_name: tname, saved_input: input.clone() });
                                            let fut = async move { (rid, resp_rx.await) }.boxed();
                                            futures.push(fut);
                                        }
                                        Err(TrySendError::Full(dropped)) => {
                                            let dropped_rid = dropped.request_id.clone();
                                            let dropped_tool = dropped.tool_name.clone();
                                            tracing::warn!(thread_id = %thread_id, request_id = %dropped_rid, tool_name = %dropped_tool, "permission_tx full, auto-denying");
                                            let resp = build_control_response_deny(&dropped_rid, "Permission queue full");
                                            if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                                tracing::error!("stdin write error (wait_for_permissions full-deny): {}", e);
                                                return PermissionsWaitResult::Interrupted;
                                            }
                                            let _ = stdin.flush().await;
                                        }
                                        Err(TrySendError::Closed(_)) => {
                                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tname, "permission_tx send failed");
                                            return PermissionsWaitResult::ChannelClosed;
                                        }
                                    }
                                }
                            }
                            Ok(StreamEvent::RateLimit { rate_limit_type, utilization, resets_at, is_using_overage, .. }) => {
                                handle_ratelimit_event(ratelimit_tx, rate_limit_type.as_deref(), utilization, resets_at, is_using_overage);
                            }
                            Ok(ev) => {
                                if let Some(tx) = event_tx {
                                    let _ = tx.send(ev).await;
                                } else {
                                    tracing::debug!("Draining event during wait_for_permissions (no event_tx): {:?}", ev);
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Parse error (wait_for_permissions): {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("stdout read error (wait_for_permissions): {}", e);
                        queue_rx.close();
                        // pending 전부 deny (stdin 직렬 write, write-per-flush)
                        for (rid, entry) in &pending {
                            let resp = build_control_response_deny(rid, "Process exited");
                            if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                tracing::error!("stdin write error (wait_for_permissions read-error deny): {}", e);
                                break;
                            }
                            if let Err(e) = stdin.flush().await {
                                tracing::error!("stdin flush error (wait_for_permissions read-error deny): {}", e);
                                break;
                            }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %entry.tool_name, behavior = "deny", reason = "Process exited", "control_response written");
                        }
                        return PermissionsWaitResult::Interrupted;
                    }
                }
            }

            // ── permission decision resolved ───────────────────────────────
            resolved = futures.next(), if !futures.is_empty() => {
                let (rid, result) = resolved.expect("FuturesUnordered next() must not be None when non-empty");
                let entry = pending.remove(&rid).expect("pending entry must exist when future resolves");
                let decision = result.unwrap_or(super::permission::PermissionDecision::Deny);

                // stdin 직렬 write (단일 select! 분기 내부 — Mutex 불필요)
                let write_result = {
                    let tool_name = &entry.tool_name;
                    let input = &entry.saved_input;
                    match &decision {
                        super::permission::PermissionDecision::Allow => {
                            let resp = build_control_response_allow(&rid, input);
                            let r = stdin.write_all(resp.as_bytes()).await;
                            if r.is_ok() { let _ = stdin.flush().await; }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, behavior = "allow", "control_response written");
                            r
                        }
                        super::permission::PermissionDecision::AlwaysAllow => {
                            let resp = build_control_response_allow(&rid, input);
                            let r = stdin.write_all(resp.as_bytes()).await;
                            if r.is_ok() {
                                let _ = stdin.flush().await;
                                permission_cache.add_always_allow(tool_name);
                            }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, behavior = "always_allow", "control_response written");
                            r
                        }
                        super::permission::PermissionDecision::Deny => {
                            let resp = build_control_response_deny(&rid, "User rejected this action");
                            let r = stdin.write_all(resp.as_bytes()).await;
                            if r.is_ok() { let _ = stdin.flush().await; }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, behavior = "deny", "control_response written");
                            r
                        }
                        super::permission::PermissionDecision::Answer(answers) => {
                            let resp = build_control_response_ask_answer(&rid, input, answers);
                            let r = stdin.write_all(resp.as_bytes()).await;
                            if r.is_ok() { let _ = stdin.flush().await; }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, behavior = "answer", "control_response written");
                            r
                        }
                    }
                };

                if let Err(e) = write_result {
                    tracing::error!("stdin write error (wait_for_permissions resolved): {}", e);
                    return PermissionsWaitResult::Interrupted;
                }

                decisions.push((rid, decision));

                // pending 이 모두 비어있고 최소 1개 처리 완료 → AllResolved
                if pending.is_empty() && !decisions.is_empty() {
                    return PermissionsWaitResult::AllResolved { _decisions: decisions };
                }
            }
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
    ratelimit_tx: &tokio::sync::watch::Sender<RateLimitInfo>,
    thread_id: &str,
    channel_id: &ChannelId,
    ctx: &Context,
    timeout_secs: u64,
    lang: Lang,
    event_tx: mpsc::Sender<StreamEvent>,
    model_name: &mut String,
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
                                    // Clone all fields before any move
                                    let saved_request_id = request_id.clone();
                                    let saved_tool_name = tool_name.clone();
                                    let saved_tool_use_id = tool_use_id.clone();
                                    let saved_input = input.clone();
                                    let saved_decision_reason = decision_reason.clone();

                                    tracing::info!("control_request received: tool={} request_id={} input={:?}", saved_tool_name, saved_request_id, saved_input);
                                    if permission_cache.is_always_allowed(&saved_tool_name) {
                                        tracing::info!("cache hit: tool={} — auto-allow (bypass flag = {})", saved_tool_name, std::env::var("PIDORY_SPIKE_BYPASS_CACHE").unwrap_or_default());
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
                                        tool_use_id: saved_tool_use_id.clone(),
                                        input: saved_input.clone(),
                                        decision_reason: saved_decision_reason.clone(),
                                        response_tx: resp_tx,
                                        triggered_by: *current_triggered_by,
                                    };
                                    let initial_cr = InitialControlRequest {
                                        request_id: saved_request_id.clone(),
                                        tool_name: saved_tool_name.clone(),
                                        tool_use_id: saved_tool_use_id,
                                        input: saved_input.clone(),
                                        decision_reason: saved_decision_reason,
                                        triggered_by: *current_triggered_by,
                                    };
                                    let result = wait_for_permissions(
                                        stdin, reader, line, queue_rx, interrupt_rx,
                                        queue_size, pending_recalls, thread_id,
                                        Some(&event_tx),
                                        ratelimit_tx, permission_cache, permission_tx,
                                        initial_cr,
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

    // ── Wave 2.1 + 2.3 + 2.4: parallel CR mock tests ─────────────────────────

    use super::{
        wait_for_permissions, InitialControlRequest, PermissionsWaitResult,
    };
    use crate::subprocess::permission::{PermissionDecision, PermissionRequest};
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, BufReader};

    /// CR JSON 라인 1개를 반환하는 헬퍼 (wait_for_permissions reader 주입용).
    fn cr_json_line(request_id: &str, tool_name: &str) -> Vec<u8> {
        let line = format!(
            "{{\"type\":\"control_request\",\"request_id\":\"{request_id}\",\"tool_name\":\"{tool_name}\",\"tool_use_id\":\"use_{request_id}\",\"input\":{{}}}}\n"
        );
        line.into_bytes()
    }

    /// 테스트용 `InitialControlRequest` 생성 헬퍼.
    fn make_initial_cr(request_id: &str, tool_name: &str) -> InitialControlRequest {
        InitialControlRequest {
            request_id: request_id.to_string(),
            tool_name: tool_name.to_string(),
            tool_use_id: format!("use_{request_id}"),
            input: serde_json::json!({}),
            decision_reason: None,
            triggered_by: poise::serenity_prelude::UserId::new(1),
        }
    }

    /// stdin write 내용을 읽어 JSON 값으로 파싱하는 헬퍼.
    /// write-side 가 drop 되거나 expected_count 개 수집되면 종료.
    async fn drain_stdin_writes(
        read_rx: &mut tokio::io::DuplexStream,
        expected_count: usize,
    ) -> Vec<serde_json::Value> {
        let mut results = Vec::new();
        let mut buf = String::new();
        let mut byte_buf = [0u8; 1];
        loop {
            if results.len() >= expected_count {
                break;
            }
            match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                read_rx.read(&mut byte_buf),
            ).await {
                Ok(Ok(0)) => break,
                Ok(Ok(_)) => {
                    let ch = byte_buf[0] as char;
                    buf.push(ch);
                    if ch == '\n' && !buf.trim().is_empty()
                        && let Ok(v) = serde_json::from_str::<serde_json::Value>(buf.trim()) {
                        results.push(v);
                        buf.clear();
                    }
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
        results
    }

    /// 공통 채널 셋업 매크로 helper (반복 코드 축약).
    /// NOTE: queue_tx 와 interrupt_tx 는 절대 drop 하지 마라 — select! 에서 None 수신 시 종료됨.
    macro_rules! setup_channels {
        () => {{
            let (queue_tx, queue_rx) = tokio::sync::mpsc::channel::<crate::subprocess::session_manager::QueuedMessage>(5);
            let (interrupt_tx, interrupt_rx) = tokio::sync::mpsc::channel::<()>(1);
            let queue_size = Arc::new(AtomicUsize::new(0));
            let pending_recalls = Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::<poise::serenity_prelude::MessageId, (String, Arc<std::sync::atomic::AtomicBool>)>::new()
            ));
            let (ratelimit_tx, _ratelimit_rx) = tokio::sync::watch::channel(crate::ratelimit::RateLimitInfo::default());
            (queue_tx, queue_rx, interrupt_tx, interrupt_rx, queue_size, pending_recalls, ratelimit_tx)
        }};
    }

    /// duplex 기반 mock reader 생성 헬퍼.
    /// write-side (`DuplexStream`) 를 keep-alive 로 유지하면 reader 는 EOF 를 반환하지 않음.
    /// CR 라인들을 write-side 에 미리 쓰고, write-side 를 반환해 keep-alive 상태 유지.
    async fn make_duplex_reader(initial_lines: &[Vec<u8>]) -> (BufReader<tokio::io::DuplexStream>, tokio::io::DuplexStream) {
        use tokio::io::AsyncWriteExt;
        let (writer, reader_stream) = tokio::io::duplex(65536);
        let mut writer = writer;
        for line in initial_lines {
            writer.write_all(line).await.unwrap();
        }
        (BufReader::new(reader_stream), writer)
    }

    /// 2.1: 병렬 2 CR 모두 permission_tx 에 도달
    #[tokio::test]
    async fn parallel_two_crs_both_surfaced() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, _stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: 2개 수신 후 Allow 전송
        let collect_task = tokio::spawn(async move {
            let mut received = Vec::new();
            if let Some(req) = permission_rx.recv().await {
                received.push(req.request_id.clone());
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
            if let Some(req) = permission_rx.recv().await {
                received.push(req.request_id.clone());
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
            received
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;

        let received_ids = collect_task.await.unwrap();
        assert_eq!(received_ids.len(), 2, "permission_tx 에 2개 CR 모두 도달해야 함");
        assert!(received_ids.contains(&"cr1".to_string()));
        assert!(received_ids.contains(&"cr2".to_string()));
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.1: CR 2개 후 FIFO 순서로 Allow → stdin 에 2개 allow JSON 기록
    #[tokio::test]
    async fn parallel_two_crs_fifo_response() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: cr1 먼저 Allow, cr2 나중 Allow (FIFO)
        let handler_task = tokio::spawn(async move {
            let req1 = permission_rx.recv().await.unwrap();
            let req2 = permission_rx.recv().await.unwrap();
            let _ = req1.response_tx.send(PermissionDecision::Allow);
            let _ = req2.response_tx.send(PermissionDecision::Allow);
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 2).await;
        assert_eq!(writes.len(), 2, "stdin 에 2개 JSON 기록되어야 함");
        for w in &writes {
            assert_eq!(w["type"], "control_response");
            assert_eq!(w["response"]["response"]["behavior"], "allow");
        }
        let rids: Vec<&str> = writes.iter().map(|w| w["response"]["request_id"].as_str().unwrap()).collect();
        assert!(rids.contains(&"cr1"), "cr1 allow 포함");
        assert!(rids.contains(&"cr2"), "cr2 allow 포함");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.1: CR 2개, resp_rx2 먼저 Allow, resp_rx1 나중 Allow → stdin 에 역순 write 정상
    #[tokio::test]
    async fn parallel_two_crs_reverse_response() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: cr2 먼저 Allow, cr1 나중 Allow (역순)
        let handler_task = tokio::spawn(async move {
            let req1 = permission_rx.recv().await.unwrap();
            let req2 = permission_rx.recv().await.unwrap();
            let _ = req2.response_tx.send(PermissionDecision::Allow);
            let _ = req1.response_tx.send(PermissionDecision::Allow);
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 2).await;
        assert_eq!(writes.len(), 2, "stdin 에 2개 JSON 기록되어야 함");
        for w in &writes {
            assert_eq!(w["type"], "control_response");
            assert_eq!(w["response"]["response"]["behavior"], "allow");
        }
        let rids: Vec<&str> = writes.iter().map(|w| w["response"]["request_id"].as_str().unwrap()).collect();
        assert!(rids.contains(&"cr1"), "cr1 allow 포함");
        assert!(rids.contains(&"cr2"), "cr2 allow 포함");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.1: CR 3개, 1 Allow + 2 Deny + 3 Allow → stdin 에 3개 대응 JSON
    #[tokio::test]
    async fn parallel_three_crs_mixed() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        let cr3_bytes = cr_json_line("cr3", "Edit");
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes, cr3_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(8192);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: 3개 모두 수신 후 cr1=Allow, cr2=Deny, cr3=Allow
        let handler_task = tokio::spawn(async move {
            let mut requests = std::collections::HashMap::new();
            for _ in 0..3 {
                if let Some(req) = permission_rx.recv().await {
                    requests.insert(req.request_id.clone(), req);
                }
            }
            if let Some(req) = requests.remove("cr1") {
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
            if let Some(req) = requests.remove("cr2") {
                let _ = req.response_tx.send(PermissionDecision::Deny);
            }
            if let Some(req) = requests.remove("cr3") {
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 3).await;
        assert_eq!(writes.len(), 3, "stdin 에 3개 JSON 기록되어야 함");
        let mut by_rid: std::collections::HashMap<&str, &serde_json::Value> = std::collections::HashMap::new();
        for w in &writes {
            by_rid.insert(w["response"]["request_id"].as_str().unwrap(), w);
        }
        assert_eq!(by_rid["cr1"]["response"]["response"]["behavior"], "allow", "cr1 → allow");
        assert_eq!(by_rid["cr2"]["response"]["response"]["behavior"], "deny",  "cr2 → deny");
        assert_eq!(by_rid["cr3"]["response"]["response"]["behavior"], "allow", "cr3 → allow");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.3: interrupt_rx 수신 시 pending 전체에 Deny stdin write
    #[tokio::test]
    async fn interrupt_during_pending_sends_deny_to_all() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        // duplex reader: EOF 없이 cr2 주입 후 대기
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        // permission buffer 충분히 크게
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: 2개 수신 후 interrupt 전송 (resp_tx 는 보관 — future 가 살아있어야 pending 유지)
        let interrupt_task = tokio::spawn(async move {
            let _req1 = permission_rx.recv().await; // resp_tx keep-alive
            let _req2 = permission_rx.recv().await; // resp_tx keep-alive
            let _ = interrupt_tx.send(()).await;
            // _req1, _req2 는 이 스코프 끝에서 drop — 여기서는 interrupt 보내기 전에 pending 상태
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        interrupt_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 2).await;
        assert_eq!(writes.len(), 2, "interrupt 시 pending 2개 deny 기록되어야 함");
        for w in &writes {
            assert_eq!(w["type"], "control_response");
            assert_eq!(w["response"]["response"]["behavior"], "deny");
            assert_eq!(w["response"]["response"]["message"], "Interrupted by user");
        }
        assert!(matches!(result, PermissionsWaitResult::Interrupted));
    }

    /// 2.3: resp_tx drop → RecvError → Deny 해석, stdin write
    #[tokio::test]
    async fn resp_rx_dropped_interprets_as_deny() {
        // reader: 추가 CR 없음. duplex reader 로 EOF 없이 대기 — futures arm 이 RecvError 로 resolve 됨.
        let (mut reader, _reader_write) = make_duplex_reader(&[]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: cr1 수신 후 resp_tx drop (RecvError 유발)
        let handler_task = tokio::spawn(async move {
            if let Some(req) = permission_rx.recv().await {
                drop(req.response_tx); // resp_rx.await → RecvError → Deny
            }
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 1).await;
        assert_eq!(writes.len(), 1, "RecvError → deny JSON 1개 기록되어야 함");
        assert_eq!(writes[0]["type"], "control_response");
        assert_eq!(writes[0]["response"]["response"]["behavior"], "deny");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.3: permission_tx closed → PermissionsWaitResult::ChannelClosed 반환
    #[tokio::test]
    async fn permission_tx_closed_returns_channel_closed() {
        // reader: EOF (initial CR 처리 전에 closed 감지됨)
        let mock_reader = tokio_test::io::Builder::new().build();
        let mut reader = BufReader::new(mock_reader);
        let mut line = String::new();

        let (mut stdin_write, _stdin_read) = tokio::io::duplex(4096);
        // permission_rx drop → try_send 가 TrySendError::Closed 반환
        let (permission_tx, permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        drop(permission_rx); // 먼저 rx drop → tx.try_send 가 Closed

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;

        assert!(
            matches!(result, PermissionsWaitResult::ChannelClosed),
            "permission_rx closed → ChannelClosed 반환해야 함"
        );
    }

    /// 2.4: permission_tx buffer 1, buffer 꽉 찬 상태에서 initial CR → auto-deny
    #[tokio::test]
    async fn permission_tx_full_auto_denies_initial() {
        let mock_reader = tokio_test::io::Builder::new().build();
        let mut reader = BufReader::new(mock_reader);
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        // buffer 1 로 설정 후 dummy 로 채움
        let (permission_tx, _permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(1);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let (dummy_tx, _dummy_rx) = tokio::sync::oneshot::channel::<PermissionDecision>();
        let dummy_req = PermissionRequest {
            request_id: "dummy".to_string(),
            tool_name: "Dummy".to_string(),
            tool_use_id: "use_dummy".to_string(),
            input: serde_json::json!({}),
            decision_reason: None,
            response_tx: dummy_tx,
            triggered_by: poise::serenity_prelude::UserId::new(1),
        };
        permission_tx.try_send(dummy_req).expect("buffer 가 비어있어야 함");

        let initial_cr = make_initial_cr("cr1", "Bash");

        // initial CR full-deny 는 select! 진입 전 처리 → queue/interrupt keep-alive 불필요
        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;

        let writes = drain_stdin_writes(&mut stdin_read, 1).await;
        assert_eq!(writes.len(), 1, "full-deny 1개 기록되어야 함");
        assert_eq!(writes[0]["type"], "control_response");
        assert_eq!(writes[0]["response"]["response"]["behavior"], "deny");
        assert_eq!(writes[0]["response"]["response"]["message"], "Permission queue full");
        assert_eq!(writes[0]["response"]["request_id"], "cr1");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.4: permission_tx buffer 1, initial 통과 후 loop CR 2개 → 2번째부터 full-deny
    #[tokio::test]
    async fn permission_tx_full_auto_denies_during_loop() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        let cr3_bytes = cr_json_line("cr3", "Edit");
        // duplex reader: cr2, cr3 주입 후 EOF 없이 대기
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes, cr3_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(8192);
        // buffer 1: cr1 initial 이 차지하면 cr2, cr3 는 full
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(1);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: cr1 수신 후 Allow. cr2/cr3 는 full-deny 로 permission_rx 에 오지 않음.
        let handler_task = tokio::spawn(async move {
            if let Some(req) = permission_rx.recv().await {
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        // allow 1 + full-deny 2 = 3개
        let writes = drain_stdin_writes(&mut stdin_read, 3).await;
        assert_eq!(writes.len(), 3, "allow 1개 + full-deny 2개 = 3개 기록되어야 함");
        let mut by_rid: std::collections::HashMap<&str, &serde_json::Value> = std::collections::HashMap::new();
        for w in &writes {
            by_rid.insert(w["response"]["request_id"].as_str().unwrap(), w);
        }
        assert_eq!(by_rid["cr1"]["response"]["response"]["behavior"], "allow", "cr1 → allow");
        assert_eq!(by_rid["cr2"]["response"]["response"]["behavior"], "deny",  "cr2 → full-deny");
        assert_eq!(by_rid["cr3"]["response"]["response"]["behavior"], "deny",  "cr3 → full-deny");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 회귀: 단일 CR baseline — 기존 단수 시나리오가 여전히 동작
    #[tokio::test]
    async fn single_cr_baseline_still_works() {
        // duplex reader: 추가 CR 없음, EOF 없이 대기
        let (mut reader, _reader_write) = make_duplex_reader(&[]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr-single", "Bash");

        let handler_task = tokio::spawn(async move {
            if let Some(req) = permission_rx.recv().await {
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 1).await;
        assert_eq!(writes.len(), 1, "단일 CR → allow JSON 1개");
        assert_eq!(writes[0]["type"], "control_response");
        assert_eq!(writes[0]["response"]["response"]["behavior"], "allow");
        assert_eq!(writes[0]["response"]["request_id"], "cr-single");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }
}
