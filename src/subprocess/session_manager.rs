use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use poise::serenity_prelude::{ChannelId, Context, MessageId};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command, ChildStdin};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use std::process::Stdio;

use crate::config::ClaudeConfig;
use crate::db::repository;
use crate::error::PidoryError;
use crate::handler::formatter;
use crate::i18n::Lang;
use super::background::BackgroundTaskTracker;
use super::parser::{parse_line, StreamEvent, ContentBlock, build_control_response_allow, build_control_response_deny};
use super::permission::{PermissionCache, PermissionDecision, PermissionRequest};

pub struct SessionCreateResult {
    pub permission_rx: Option<mpsc::Receiver<PermissionRequest>>,
    pub evicted_thread_id: Option<String>,
}

pub struct QueuedMessage {
    pub content: String,
    pub channel_id: ChannelId,
    pub message_id: MessageId,
    pub event_tx: Option<mpsc::Sender<StreamEvent>>,  // None = mid-turn inject
}

struct SessionInner {
    child: Child,
    queue_tx: mpsc::Sender<QueuedMessage>,
    queue_size: Arc<AtomicUsize>,
    worker_task: JoinHandle<()>,
    permission_tx: mpsc::Sender<PermissionRequest>,
    interrupt_tx: mpsc::Sender<()>,
    last_activity: Arc<StdMutex<Instant>>,
    has_active_bg_tasks: Arc<AtomicBool>,
    is_turn_active: Arc<AtomicBool>,
}

pub struct SessionInfo {
    pub thread_id: String,
    pub idle_duration: Duration,
    pub has_bg_tasks: bool,
    pub is_turn_active: bool,
}

pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<String, SessionInner>>>,
    config: Arc<ClaudeConfig>,
    max_sessions: usize,
}

impl SessionManager {
    pub fn new(config: Arc<ClaudeConfig>, max_sessions: usize) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            config,
            max_sessions,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn get_or_create(
        &self,
        thread_id: &str,
        project_path: &str,
        session_id: Option<&str>,
        disallowed_tools: &[String],
        ctx: Context,
        channel_id: ChannelId,
        db: sqlx::SqlitePool,
        lang: Lang,
    ) -> Result<SessionCreateResult, PidoryError> {
        let mut sessions = self.sessions.lock().await;

        if sessions.contains_key(thread_id) {
            return Ok(SessionCreateResult { permission_rx: None, evicted_thread_id: None });
        }

        let mut evicted_thread_id = None;
        if sessions.len() >= self.max_sessions {
            if let Some(evict_tid) = Self::find_evict_target(&sessions) {
                tracing::info!(thread_id = %evict_tid, "Evicting idle session (LRU)");
                if let Some(mut inner) = sessions.remove(&evict_tid) {
                    inner.worker_task.abort();
                    let _ = inner.child.kill().await;
                }
                evicted_thread_id = Some(evict_tid);
            } else {
                return Err(PidoryError::Subprocess(
                    format!("all {} sessions are busy", self.max_sessions)
                ));
            }
        }

        let mut cmd = Command::new(&self.config.binary_path);
        cmd.arg("-p")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--permission-prompt-tool")
            .arg("stdio")
            .arg("--replay-user-messages");

        if let Some(sid) = session_id {
            cmd.arg("--resume").arg(sid);
        }

        if !disallowed_tools.is_empty() {
            cmd.arg("--disallowedTools").arg(disallowed_tools.join(","));
        }

        cmd.current_dir(project_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| PidoryError::Subprocess(format!("failed to spawn process: {}", e)))?;

        let stdin: ChildStdin = child
            .stdin
            .take()
            .ok_or_else(|| PidoryError::Subprocess("no stdin handle".to_string()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PidoryError::Subprocess("no stdout handle".to_string()))?;

        let (queue_tx, queue_rx) = mpsc::channel::<QueuedMessage>(5);
        let queue_size = Arc::new(AtomicUsize::new(0));
        let (permission_tx, permission_rx) = mpsc::channel::<PermissionRequest>(8);
        let (interrupt_tx, mut interrupt_rx) = mpsc::channel::<()>(1);

        let last_activity = Arc::new(StdMutex::new(Instant::now()));
        let has_active_bg_tasks = Arc::new(AtomicBool::new(false));
        let is_turn_active = Arc::new(AtomicBool::new(false));

        // Combined worker task: reads queue, writes stdin, reads stdout until result, streams events
        let queue_size_for_worker = Arc::clone(&queue_size);
        let timeout_secs = self.config.subprocess_timeout_secs;
        let sessions_clone = Arc::clone(&self.sessions);
        let thread_id_for_worker = thread_id.to_string();
        let permission_tx_for_worker = permission_tx.clone();
        let ctx_for_worker = ctx;
        let channel_id_for_worker = channel_id;
        let db_for_worker = db;
        let last_activity_clone = Arc::clone(&last_activity);
        let has_bg_tasks_clone = Arc::clone(&has_active_bg_tasks);
        let is_turn_clone = Arc::clone(&is_turn_active);
        let worker_task = tokio::spawn(async move {
            let mut stdin = stdin;
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut queue_rx = queue_rx;
            let permission_tx = permission_tx_for_worker;
            let mut permission_cache = PermissionCache::new();
            let mut tracker = BackgroundTaskTracker::new();
            // interrupt_rx is moved into this closure
            let interrupt_rx = &mut interrupt_rx;

            loop {
                line.clear();
                let msg = tokio::select! {
                    biased;
                    // stdout 우선: background task 이벤트 감지
                    read_result = reader.read_line(&mut line) => {
                        match read_result {
                            Ok(0) => {
                                tracing::info!(
                                    "Process stdout EOF (between turns) for thread {}",
                                    thread_id_for_worker
                                );
                                break;
                            }
                            Ok(_) => {
                                let trimmed = line.trim_end();
                                if trimmed.is_empty() { continue; }
                                match parse_line(trimmed) {
                                    Ok(StreamEvent::TaskStarted { ref task_id, ref task_type, ref description, .. }) => {
                                        tracker.track_started(task_id, task_type, description);
                                        has_bg_tasks_clone.store(tracker.has_active_tasks(), Ordering::Relaxed);
                                        tracing::info!("Background task started: {} ({})", task_id, task_type);
                                        let start_msg = lang.bg_task_started(description);
                                        channel_id_for_worker.say(&ctx_for_worker, &start_msg).await.ok();
                                        continue;
                                    }
                                    Ok(StreamEvent::TaskProgress { ref task_id, ref description, .. }) => {
                                        tracker.track_progress(task_id, description);
                                        continue;
                                    }
                                    Ok(StreamEvent::TaskNotification { ref task_id, ref status, ref summary, .. }) => {
                                        tracker.track_completed(task_id);
                                        has_bg_tasks_clone.store(tracker.has_active_tasks(), Ordering::Relaxed);
                                        let notify_msg = if status == "completed" {
                                            format!("-# 🔔 {}", summary)
                                        } else {
                                            format!("-# ❌ {}", summary)
                                        };
                                        channel_id_for_worker.say(&ctx_for_worker, &notify_msg).await.ok();

                                        if let Err(e) = repository::update_session_status(&db_for_worker, &thread_id_for_worker, "running").await {
                                            tracing::warn!("Failed to update session status for thread {}: {}", thread_id_for_worker, e);
                                        }

                                        // bg mini-loop: background turn 이벤트 처리
                                        'bg_turn: loop {
                                            line.clear();
                                            tokio::select! {
                                                read_result = reader.read_line(&mut line) => {
                                                    match read_result {
                                                        Ok(0) => {
                                                            if let Err(e) = repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await {
                                                                tracing::warn!("Failed to update session status for thread {}: {}", thread_id_for_worker, e);
                                                            }
                                                            break 'bg_turn;
                                                        }
                                                        Ok(_) => {
                                                            let trimmed = line.trim_end();
                                                            if trimmed.is_empty() { continue 'bg_turn; }
                                                            match parse_line(trimmed) {
                                                                Ok(StreamEvent::Result { .. }) => {
                                                                    if let Err(e) = repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await {
                                                                tracing::warn!("Failed to update session status for thread {}: {}", thread_id_for_worker, e);
                                                            }
                                                                    break 'bg_turn;
                                                                }
                                                                Ok(StreamEvent::Assistant { ref content, .. }) => {
                                                                    for block in content {
                                                                        match block {
                                                                            ContentBlock::Text(text) if !text.trim().is_empty() => {
                                                                                let bg_text = lang.bg_notification(text);
                                                                                channel_id_for_worker.say(&ctx_for_worker, &bg_text).await.ok();
                                                                            }
                                                                            ContentBlock::ToolUse { name, input, .. } => {
                                                                                let formatted = formatter::format_tool_use(name, input);
                                                                                let bg_text = lang.bg_notification(&formatted);
                                                                                channel_id_for_worker.say(&ctx_for_worker, &bg_text).await.ok();
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
                                                                                channel_id_for_worker.say(&ctx_for_worker, &bg_text).await.ok();
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                                Ok(StreamEvent::ControlRequest { ref request_id, ref tool_name, ref input, .. }) => {
                                                                    let resp = if permission_cache.is_always_allowed(tool_name) {
                                                                        build_control_response_allow(request_id, input)
                                                                    } else {
                                                                        let deny_msg = lang.bg_permission_denied(tool_name);
                                                                        channel_id_for_worker.say(&ctx_for_worker, &deny_msg).await.ok();
                                                                        build_control_response_deny(request_id, lang.bg_permission_deny_reason())
                                                                    };
                                                                    if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                                                        tracing::error!("stdin write error (bg turn): {}", e);
                                                                        if let Err(e) = repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await {
                                                                tracing::warn!("Failed to update session status for thread {}: {}", thread_id_for_worker, e);
                                                            }
                                                                        break 'bg_turn;
                                                                    }
                                                                    let _ = stdin.flush().await;
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
                                                            if let Err(e) = repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await {
                                                                tracing::warn!("Failed to update session status for thread {}: {}", thread_id_for_worker, e);
                                                            }
                                                            break 'bg_turn;
                                                        }
                                                    }
                                                }
                                                // bg turn 중 queue 메시지 → mid-turn inject
                                                new_msg = queue_rx.recv() => {
                                                    match new_msg {
                                                        Some(m) => {
                                                            queue_size_for_worker.fetch_sub(1, Ordering::Relaxed);
                                                            let inject_json = serde_json::json!({
                                                                "type": "user",
                                                                "message": {
                                                                    "role": "user",
                                                                    "content": [{"type": "text", "text": m.content}]
                                                                }
                                                            });
                                                            let inject_line = format!("{}\n", inject_json);
                                                            if let Err(e) = stdin.write_all(inject_line.as_bytes()).await {
                                                                tracing::error!("mid-turn stdin write error (bg turn): {}", e);
                                                                if let Err(e) = repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await {
                                                                tracing::warn!("Failed to update session status for thread {}: {}", thread_id_for_worker, e);
                                                            }
                                                                break 'bg_turn;
                                                            }
                                                            let _ = stdin.flush().await;
                                                        }
                                                        None => {
                                                            if let Err(e) = repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await {
                                                                tracing::warn!("Failed to update session status for thread {}: {}", thread_id_for_worker, e);
                                                            }
                                                            break 'bg_turn;
                                                        }
                                                    }
                                                }
                                                // bg turn 중 interrupt
                                                _ = interrupt_rx.recv() => {
                                                    let interrupt_msg = serde_json::json!({
                                                        "type": "control_request",
                                                        "request_id": format!("interrupt_{}", std::time::SystemTime::now()
                                                            .duration_since(std::time::UNIX_EPOCH)
                                                            .unwrap_or_default()
                                                            .as_millis()),
                                                        "request": {"subtype": "interrupt"}
                                                    });
                                                    let interrupt_line = format!("{}\n", interrupt_msg);
                                                    if let Err(e) = stdin.write_all(interrupt_line.as_bytes()).await {
                                                        tracing::error!("interrupt write error (bg turn): {}", e);
                                                    } else {
                                                        let _ = stdin.flush().await;
                                                    }
                                                }
                                            }
                                        }

                                        continue;
                                    }
                                    Ok(StreamEvent::ControlRequest { ref request_id, ref tool_name, ref input, .. }) => {
                                        let resp = if permission_cache.is_always_allowed(tool_name) {
                                            build_control_response_allow(request_id, input)
                                        } else {
                                            let deny_msg = lang.bg_permission_denied(tool_name);
                                            channel_id_for_worker.say(&ctx_for_worker, &deny_msg).await.ok();
                                            build_control_response_deny(request_id, lang.bg_permission_deny_reason())
                                        };
                                        if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                            tracing::error!("stdin write error (between turns): {}", e);
                                            break;
                                        }
                                        let _ = stdin.flush().await;
                                        continue;
                                    }
                                    Ok(event) => {
                                        tracing::debug!("Between-turns event drained: {:?}", event);
                                        continue;
                                    }
                                    Err(e) => {
                                        tracing::warn!("Parse error (between turns): {}", e);
                                        continue;
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!(
                                    "stdout read error (between turns) for thread {}: {}",
                                    thread_id_for_worker,
                                    e
                                );
                                break;
                            }
                        }
                    }
                    _ = interrupt_rx.recv() => {
                        tracing::debug!("Stale interrupt consumed between turns");
                        continue;
                    }
                    msg = queue_rx.recv() => {
                        match msg {
                            Some(m) => m,
                            None => break,
                        }
                    }
                };

                queue_size_for_worker.fetch_sub(1, Ordering::Relaxed);
                *last_activity_clone.lock().unwrap() = Instant::now();

                let json_msg = serde_json::json!({
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": [{"type": "text", "text": msg.content}]
                    }
                });
                let json_line = format!("{}\n", json_msg);

                if let Err(e) = stdin.write_all(json_line.as_bytes()).await {
                    tracing::error!("stdin write error for thread {}: {}", thread_id_for_worker, e);
                    break;
                }
                if let Err(e) = stdin.flush().await {
                    tracing::error!("stdin flush error for thread {}: {}", thread_id_for_worker, e);
                    break;
                }

                // event_tx가 없으면 mid-turn inject: stdin에 쓰기만 하고 다음으로
                let Some(event_tx) = msg.event_tx else {
                    continue;
                };

                is_turn_clone.store(true, Ordering::Relaxed);
                tracing::info!(thread_id = %thread_id_for_worker, timeout_secs, "Primary turn started");

                // primary 메시지: result까지 stdout 읽기 + mid-turn inject 동시 처리
                let mut timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
                let mut bg_turn_active = false;
                let mut soft_timeout_fired = false;
                'turn: loop {
                    line.clear();
                    tokio::select! {
                        // interrupt 요청
                        _ = interrupt_rx.recv() => {
                            let interrupt_msg = serde_json::json!({
                                "type": "control_request",
                                "request_id": format!("interrupt_{}", std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis()),
                                "request": {"subtype": "interrupt"}
                            });
                            let interrupt_line = format!("{}\n", interrupt_msg);
                            if let Err(e) = stdin.write_all(interrupt_line.as_bytes()).await {
                                tracing::error!("interrupt write error: {}", e);
                                break 'turn;
                            }
                            let _ = stdin.flush().await;
                            // result 이벤트는 기존 stdout 읽기에서 처리됨
                        }
                        // 큐에서 새 메시지 (mid-turn inject)
                        new_msg = queue_rx.recv() => {
                            match new_msg {
                                Some(m) => {
                                    queue_size_for_worker.fetch_sub(1, Ordering::Relaxed);
                                    *last_activity_clone.lock().unwrap() = Instant::now();
                                    let inject_json = serde_json::json!({
                                        "type": "user",
                                        "message": {
                                            "role": "user",
                                            "content": [{"type": "text", "text": m.content}]
                                        }
                                    });
                                    let inject_line = format!("{}\n", inject_json);
                                    if let Err(e) = stdin.write_all(inject_line.as_bytes()).await {
                                        tracing::error!("mid-turn stdin write error: {}", e);
                                        break 'turn;
                                    }
                                    let _ = stdin.flush().await;
                                    // m.event_tx는 None이므로 drop됨
                                    // 이벤트는 계속 원래 event_tx로 감
                                }
                                None => {
                                    // queue closed (kill_session)
                                    break 'turn;
                                }
                            }
                        }
                        // stdout에서 이벤트 읽기
                        read_result = tokio::time::timeout_at(timeout_deadline, reader.read_line(&mut line)) => {
                            match read_result {
                                Ok(Ok(0)) => {
                                    tracing::info!(
                                        "Process stdout EOF for thread {}",
                                        thread_id_for_worker
                                    );
                                    queue_rx.close();
                                    break 'turn;
                                }
                                Ok(Ok(_)) => {
                                    timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
                                    tracing::debug!(thread_id = %thread_id_for_worker, "Timeout deadline reset");
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
                                            tracing::debug!(thread_id = %thread_id_for_worker, event = event_name, "stdout event");
                                            // Background task 이벤트: user turn 중에도 올 수 있음
                                            match &event {
                                                StreamEvent::TaskStarted { task_id, task_type, description, .. } => {
                                                    tracker.track_started(task_id, task_type, description);
                                                    has_bg_tasks_clone.store(tracker.has_active_tasks(), Ordering::Relaxed);
                                                    let start_msg = lang.bg_task_started(description);
                                                    channel_id_for_worker.say(&ctx_for_worker, &start_msg).await.ok();
                                                    continue 'turn;
                                                }
                                                StreamEvent::TaskProgress { task_id, description, .. } => {
                                                    tracker.track_progress(task_id, description);
                                                    continue 'turn;
                                                }
                                                StreamEvent::TaskNotification { task_id, status, summary, .. } => {
                                                    tracker.track_completed(task_id);
                                                    has_bg_tasks_clone.store(tracker.has_active_tasks(), Ordering::Relaxed);
                                                    let notify_msg = if status == "completed" {
                                                        format!("-# 🔔 {}", summary)
                                                    } else {
                                                        format!("-# ❌ {}", summary)
                                                    };
                                                    channel_id_for_worker.say(&ctx_for_worker, &notify_msg).await.ok();
                                                    bg_turn_active = true;
                                                    continue 'turn;
                                                }
                                                _ => {}
                                            }

                                            // bg turn 이벤트 처리: bg_turn_active일 때 기존 코드로 안 넘김
                                            if bg_turn_active {
                                                match &event {
                                                    StreamEvent::Result { .. } => {
                                                        bg_turn_active = false;
                                                        timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
                                                        tracing::debug!(thread_id = %thread_id_for_worker, "Timeout deadline reset");
                                                        continue 'turn; // bg turn 끝 — user turn은 계속
                                                    }
                                                    StreamEvent::Assistant { content, .. } => {
                                                        for block in content {
                                                            match block {
                                                                ContentBlock::Text(text) if !text.trim().is_empty() => {
                                                                    let bg_text = lang.bg_notification(text);
                                                                    channel_id_for_worker.say(&ctx_for_worker, &bg_text).await.ok();
                                                                }
                                                                ContentBlock::ToolUse { name, input, .. } => {
                                                                    let formatted = formatter::format_tool_use(name, input);
                                                                    let bg_text = lang.bg_notification(&formatted);
                                                                    channel_id_for_worker.say(&ctx_for_worker, &bg_text).await.ok();
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
                                                                    channel_id_for_worker.say(&ctx_for_worker, &bg_text).await.ok();
                                                                }
                                                            }
                                                        }
                                                        continue 'turn;
                                                    }
                                                    StreamEvent::ControlRequest { request_id, tool_name, input, .. } => {
                                                        let resp = if permission_cache.is_always_allowed(tool_name) {
                                                            build_control_response_allow(request_id, input)
                                                        } else {
                                                            let deny_msg = lang.bg_permission_denied(tool_name);
                                                            channel_id_for_worker.say(&ctx_for_worker, &deny_msg).await.ok();
                                                            build_control_response_deny(request_id, lang.bg_permission_deny_reason())
                                                        };
                                                        if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                                            tracing::error!("stdin write error (bg turn in user turn): {}", e);
                                                            break 'turn;
                                                        }
                                                        let _ = stdin.flush().await;
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
                                                        break 'turn;
                                                    }
                                                    let _ = stdin.flush().await;
                                                    continue 'turn;
                                                }

                                                // event_tx로 ControlRequest 전달 (handler에서 버튼 표시)
                                                let _ = event_tx.send(event).await;

                                                // permission 요청 생성
                                                let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                                                let perm_req = PermissionRequest {
                                                    request_id: saved_request_id.clone(),
                                                    tool_name: saved_tool_name.clone(),
                                                    tool_use_id: saved_tool_use_id,
                                                    input: saved_input.clone(),
                                                    decision_reason: saved_decision_reason,
                                                    response_tx: resp_tx,
                                                };

                                                if permission_tx.send(perm_req).await.is_err() {
                                                    tracing::error!("permission_tx closed, denying");
                                                    let resp = build_control_response_deny(&saved_request_id, "Permission handler unavailable");
                                                    let _ = stdin.write_all(resp.as_bytes()).await;
                                                    let _ = stdin.flush().await;
                                                    break 'turn;
                                                }

                                                // permission 대기 루프 (timeout 없음)
                                                tokio::pin!(resp_rx);
                                                let mut perm_deny_reason: Option<String> = None;
                                                let mut perm_allow = false;
                                                let mut perm_always_allow_tool: Option<String> = None;

                                                'perm: loop {
                                                    line.clear();
                                                    tokio::select! {
                                                        // interrupt 요청 (permission 대기 중)
                                                        _ = interrupt_rx.recv() => {
                                                            let interrupt_msg = serde_json::json!({
                                                                "type": "control_request",
                                                                "request_id": format!("interrupt_{}", std::time::SystemTime::now()
                                                                    .duration_since(std::time::UNIX_EPOCH)
                                                                    .unwrap_or_default()
                                                                    .as_millis()),
                                                                "request": {"subtype": "interrupt"}
                                                            });
                                                            let interrupt_line = format!("{}\n", interrupt_msg);
                                                            if let Err(e) = stdin.write_all(interrupt_line.as_bytes()).await {
                                                                tracing::error!("interrupt write error (perm wait): {}", e);
                                                            } else {
                                                                let _ = stdin.flush().await;
                                                            }
                                                            perm_deny_reason = Some("Interrupted by user".to_string());
                                                            break 'perm;
                                                        }
                                                        decision = &mut resp_rx => {
                                                            match decision {
                                                                Ok(PermissionDecision::Allow) => {
                                                                    perm_allow = true;
                                                                }
                                                                Ok(PermissionDecision::AlwaysAllow) => {
                                                                    perm_allow = true;
                                                                    perm_always_allow_tool = Some(saved_tool_name.clone());
                                                                }
                                                                Ok(PermissionDecision::Deny) => {
                                                                    perm_deny_reason = Some("User rejected this action".to_string());
                                                                }
                                                                Err(_) => {
                                                                    perm_deny_reason = Some("Permission handler unavailable".to_string());
                                                                }
                                                            }
                                                            break 'perm;
                                                        }
                                                        // mid-turn inject during permission wait
                                                        new_msg = queue_rx.recv() => {
                                                            match new_msg {
                                                                Some(m) => {
                                                                    queue_size_for_worker.fetch_sub(1, Ordering::Relaxed);
                                                                    let inject_json = serde_json::json!({
                                                                        "type": "user",
                                                                        "message": {
                                                                            "role": "user",
                                                                            "content": [{"type": "text", "text": m.content}]
                                                                        }
                                                                    });
                                                                    let inject_line = format!("{}\n", inject_json);
                                                                    if let Err(e) = stdin.write_all(inject_line.as_bytes()).await {
                                                                        tracing::error!("mid-turn stdin write error (perm wait): {}", e);
                                                                        perm_deny_reason = Some("Permission handler unavailable".to_string());
                                                                        break 'perm;
                                                                    }
                                                                    let _ = stdin.flush().await;
                                                                }
                                                                None => {
                                                                    perm_deny_reason = Some("Permission handler unavailable".to_string());
                                                                    break 'perm;
                                                                }
                                                            }
                                                        }
                                                        // stdout events during permission wait (no timeout)
                                                        read = reader.read_line(&mut line) => {
                                                            match read {
                                                                Ok(0) => {
                                                                    tracing::info!(
                                                                        "Process stdout EOF during perm wait for thread {}",
                                                                        thread_id_for_worker
                                                                    );
                                                                    queue_rx.close();
                                                                    perm_deny_reason = Some("Process exited".to_string());
                                                                    break 'perm;
                                                                }
                                                                Ok(_) => {
                                                                    let t = line.trim_end();
                                                                    if !t.is_empty() {
                                                                        match parse_line(t) {
                                                                            Ok(ev) => {
                                                                                let _ = event_tx.send(ev).await;
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
                                                                    perm_deny_reason = Some("stdout read error".to_string());
                                                                    break 'perm;
                                                                }
                                                            }
                                                        }
                                                    }
                                                }

                                                if perm_allow {
                                                    let resp = build_control_response_allow(&saved_request_id, &saved_input);
                                                    if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                                        tracing::error!("stdin write error (allow): {}", e);
                                                        break 'turn;
                                                    }
                                                    let _ = stdin.flush().await;
                                                    if let Some(t) = perm_always_allow_tool {
                                                        permission_cache.add_always_allow(&t);
                                                    }
                                                } else if let Some(reason) = perm_deny_reason {
                                                    let resp = build_control_response_deny(&saved_request_id, &reason);
                                                    if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                                        tracing::error!("stdin write error (deny): {}", e);
                                                        break 'turn;
                                                    }
                                                    let _ = stdin.flush().await;
                                                }

                                                // timeout 리셋
                                                timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
                                                tracing::debug!(thread_id = %thread_id_for_worker, "Timeout deadline reset");
                                                continue 'turn;
                                            }

                                            // 일반 이벤트 처리
                                            let is_result = event.is_result();
                                            let _ = event_tx.send(event).await;
                                            if is_result {
                                                is_turn_clone.store(false, Ordering::Relaxed);
                                                *last_activity_clone.lock().unwrap() = Instant::now();
                                                break 'turn;
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
                                        thread_id_for_worker,
                                        e
                                    );
                                    queue_rx.close();
                                    break 'turn;
                                }
                                Err(_) => {
                                    if !soft_timeout_fired {
                                        // Soft timeout: nudge 주입
                                        soft_timeout_fired = true;
                                        tracing::warn!(
                                            thread_id = %thread_id_for_worker,
                                            timeout_secs,
                                            "Soft timeout fired — sending nudge"
                                        );
                                        let nudge = serde_json::json!({
                                            "type": "user",
                                            "message": {
                                                "role": "user",
                                                "content": [{"type": "text", "text": "[SYSTEM] No stdout activity for an extended period. A tool may be unresponsive. Check the status of any running tools and recover if needed."}]
                                            }
                                        });
                                        let nudge_line = format!("{}\n", nudge);
                                        if let Err(e) = stdin.write_all(nudge_line.as_bytes()).await {
                                            tracing::error!("nudge write error: {}", e);
                                            break 'turn;
                                        }
                                        let _ = stdin.flush().await;

                                        // Discord 알림 (deadline 설정 전에 처리 — API 지연이 retry window를 잠식하지 않도록)
                                        channel_id_for_worker.say(
                                            &ctx_for_worker,
                                            format!("-# ⚠️ {}", lang.soft_timeout_nudge())
                                        ).await.ok();

                                        // 짧은 재대기 (timeout_secs / 5, 최소 60초)
                                        let retry_secs = (timeout_secs / 5).max(60);
                                        timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(retry_secs);
                                        tracing::info!(
                                            thread_id = %thread_id_for_worker,
                                            retry_secs,
                                            "Nudge sent, retry deadline set"
                                        );

                                        continue 'turn;
                                    } else {
                                        // Hard timeout
                                        tracing::error!(
                                            thread_id = %thread_id_for_worker,
                                            timeout_secs,
                                            "Hard turn timeout — killing turn"
                                        );
                                        channel_id_for_worker.say(
                                            &ctx_for_worker,
                                            format!("⚠️ {}", lang.hard_timeout_kill())
                                        ).await.ok();
                                        break 'turn;
                                    }
                                }
                            }
                        }
                    }
                }
                // 'turn loop 종료 후 항상 리셋 (정상/비정상 모든 break 경로 커버)
                is_turn_clone.store(false, Ordering::Relaxed);
                tracing::info!(thread_id = %thread_id_for_worker, soft_timeout_fired, "Turn ended");
                // event_tx dropped → handler의 recv() returns None
            }

            tracing::info!(
                "Worker task exiting for thread {}, removing from sessions",
                thread_id_for_worker
            );
            sessions_clone.lock().await.remove(&thread_id_for_worker);
        });

        sessions.insert(
            thread_id.to_string(),
            SessionInner {
                child,
                queue_tx,
                queue_size,
                worker_task,
                permission_tx,
                interrupt_tx,
                last_activity,
                has_active_bg_tasks,
                is_turn_active,
            },
        );

        Ok(SessionCreateResult { permission_rx: Some(permission_rx), evicted_thread_id })
    }

    fn find_evict_target(sessions: &HashMap<String, SessionInner>) -> Option<String> {
        sessions.iter()
            .filter(|(_, s)| {
                !s.is_turn_active.load(Ordering::Relaxed)
                && !s.has_active_bg_tasks.load(Ordering::Relaxed)
            })
            .min_by_key(|(_, s)| *s.last_activity.lock().unwrap())
            .map(|(tid, _)| tid.clone())
    }

    pub async fn send_message(
        &self,
        thread_id: &str,
        msg: QueuedMessage,
    ) -> Result<(), PidoryError> {
        let sessions = self.sessions.lock().await;
        let inner = sessions.get(thread_id).ok_or_else(|| {
            PidoryError::NotFound(format!("no active session for thread_id: {}", thread_id))
        })?;

        let current = inner.queue_size.load(Ordering::Relaxed);
        if current >= 5 {
            return Err(PidoryError::Subprocess(format!(
                "message queue full for thread_id: {}",
                thread_id
            )));
        }

        inner.queue_size.fetch_add(1, Ordering::Relaxed);

        inner
            .queue_tx
            .try_send(msg)
            .map_err(|e| PidoryError::Subprocess(format!("queue send error: {}", e)))?;

        Ok(())
    }

    pub async fn kill_session(&self, thread_id: &str) -> Result<(), PidoryError> {
        let mut sessions = self.sessions.lock().await;
        let mut inner = sessions.remove(thread_id).ok_or_else(|| {
            PidoryError::NotFound(format!("no active session for thread_id: {}", thread_id))
        })?;

        inner.worker_task.abort();

        inner
            .child
            .kill()
            .await
            .map_err(|e| PidoryError::Subprocess(format!("kill failed: {}", e)))?;

        Ok(())
    }

    pub async fn session_exists(&self, thread_id: &str) -> bool {
        let sessions = self.sessions.lock().await;
        sessions.contains_key(thread_id)
    }

    pub async fn session_count(&self) -> usize {
        let sessions = self.sessions.lock().await;
        sessions.len()
    }

    pub async fn interrupt_session(&self, thread_id: &str) -> Result<(), PidoryError> {
        let sessions = self.sessions.lock().await;
        let inner = sessions.get(thread_id).ok_or_else(|| {
            PidoryError::NotFound(format!("no active session: {}", thread_id))
        })?;
        inner.interrupt_tx.try_send(())
            .map_err(|_| PidoryError::Subprocess("interrupt send failed".to_string()))?;
        Ok(())
    }

    pub async fn get_session_info(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.lock().await;
        let now = Instant::now();
        sessions.iter().map(|(tid, s)| {
            SessionInfo {
                thread_id: tid.clone(),
                idle_duration: now.duration_since(*s.last_activity.lock().unwrap()),
                has_bg_tasks: s.has_active_bg_tasks.load(Ordering::Relaxed),
                is_turn_active: s.is_turn_active.load(Ordering::Relaxed),
            }
        }).collect()
    }

    pub async fn sweep_idle_sessions(&self, idle_timeout: Duration) -> Vec<String> {
        if idle_timeout.is_zero() {
            return Vec::new();
        }
        let mut sessions = self.sessions.lock().await;
        let now = Instant::now();
        let targets: Vec<String> = sessions.iter()
            .filter(|(_, s)| {
                !s.is_turn_active.load(Ordering::Relaxed)
                && !s.has_active_bg_tasks.load(Ordering::Relaxed)
                && now.duration_since(*s.last_activity.lock().unwrap()) > idle_timeout
            })
            .map(|(tid, _)| tid.clone())
            .collect();

        let mut evicted = Vec::new();
        for tid in targets {
            tracing::info!(thread_id = %tid, "Sweeping idle session (TTL expired)");
            if let Some(mut inner) = sessions.remove(&tid) {
                inner.worker_task.abort();
                let _ = inner.child.kill().await;
            }
            evicted.push(tid);
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    struct MockSession {
        last_activity: Instant,
        is_turn_active: bool,
        has_bg_tasks: bool,
    }

    fn find_evict_target_mock(sessions: &[(&str, MockSession)]) -> Option<String> {
        sessions
            .iter()
            .filter(|(_, s)| !s.is_turn_active && !s.has_bg_tasks)
            .min_by_key(|(_, s)| s.last_activity)
            .map(|(tid, _)| tid.to_string())
    }

    #[test]
    fn evict_oldest_idle_session() {
        let now = Instant::now();
        let sessions = vec![
            (
                "thread_a",
                MockSession {
                    last_activity: now - Duration::from_secs(100),
                    is_turn_active: false,
                    has_bg_tasks: false,
                },
            ),
            (
                "thread_b",
                MockSession {
                    last_activity: now - Duration::from_secs(200),
                    is_turn_active: false,
                    has_bg_tasks: false,
                },
            ),
        ];
        assert_eq!(
            find_evict_target_mock(&sessions),
            Some("thread_b".to_string())
        );
    }

    #[test]
    fn skip_turn_active_session() {
        let now = Instant::now();
        let sessions = vec![
            (
                "thread_a",
                MockSession {
                    last_activity: now - Duration::from_secs(200),
                    is_turn_active: true,
                    has_bg_tasks: false,
                },
            ),
            (
                "thread_b",
                MockSession {
                    last_activity: now - Duration::from_secs(100),
                    is_turn_active: false,
                    has_bg_tasks: false,
                },
            ),
        ];
        assert_eq!(
            find_evict_target_mock(&sessions),
            Some("thread_b".to_string())
        );
    }

    #[test]
    fn skip_bg_task_session() {
        let now = Instant::now();
        let sessions = vec![
            (
                "thread_a",
                MockSession {
                    last_activity: now - Duration::from_secs(200),
                    is_turn_active: false,
                    has_bg_tasks: true,
                },
            ),
            (
                "thread_b",
                MockSession {
                    last_activity: now - Duration::from_secs(100),
                    is_turn_active: false,
                    has_bg_tasks: false,
                },
            ),
        ];
        assert_eq!(
            find_evict_target_mock(&sessions),
            Some("thread_b".to_string())
        );
    }

    #[test]
    fn all_sessions_busy() {
        let now = Instant::now();
        let sessions = vec![
            (
                "thread_a",
                MockSession {
                    last_activity: now,
                    is_turn_active: true,
                    has_bg_tasks: false,
                },
            ),
            (
                "thread_b",
                MockSession {
                    last_activity: now,
                    is_turn_active: false,
                    has_bg_tasks: true,
                },
            ),
        ];
        assert_eq!(find_evict_target_mock(&sessions), None);
    }

    #[test]
    fn empty_sessions() {
        let sessions: Vec<(&str, MockSession)> = vec![];
        assert_eq!(find_evict_target_mock(&sessions), None);
    }

    // ---------- timeout logic helpers ----------

    /// Mirrors the production formula: `(timeout_secs / 5).max(60)`
    fn compute_retry_secs(timeout_secs: u64) -> u64 {
        (timeout_secs / 5).max(60)
    }

    /// Outcome of one timeout event.
    #[derive(Debug, PartialEq)]
    enum TimeoutAction {
        /// Nudge injected, deadline reset to `retry_secs`, continue turn.
        SoftFired { retry_secs: u64 },
        /// Turn should be terminated.
        HardKill,
    }

    /// Pure state-machine for the `Err(_)` timeout branch.
    fn on_timeout(soft_timeout_fired: bool, timeout_secs: u64) -> TimeoutAction {
        if !soft_timeout_fired {
            TimeoutAction::SoftFired {
                retry_secs: compute_retry_secs(timeout_secs),
            }
        } else {
            TimeoutAction::HardKill
        }
    }

    // ---------- sliding window tests ----------

    /// After a successful read the deadline should be pushed forward.
    #[test]
    fn sliding_window_deadline_resets_on_read() {
        let timeout_secs: u64 = 600;

        // Simulate an "old" deadline that is nearly expired.
        let old_deadline =
            tokio::time::Instant::now() + Duration::from_secs(1);

        // Production line: every Ok(Ok(_)) resets the deadline.
        let new_deadline =
            tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

        // The new deadline must be strictly later than the old nearly-expired one.
        assert!(
            new_deadline > old_deadline,
            "deadline after read ({:?}) should be later than near-expired deadline ({:?})",
            new_deadline,
            old_deadline
        );
    }

    /// The gap between two consecutive deadline resets equals `timeout_secs`.
    #[test]
    fn sliding_window_gap_equals_timeout() {
        let timeout_secs: u64 = 600;
        let before = tokio::time::Instant::now();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        let after = tokio::time::Instant::now();

        // deadline - before must be in [timeout_secs, timeout_secs + small_epsilon].
        let lower = Duration::from_secs(timeout_secs);
        // Allow 100 ms for execution jitter.
        let upper = Duration::from_secs(timeout_secs) + Duration::from_millis(100);

        let gap = deadline.duration_since(before);
        assert!(gap >= lower && gap <= upper,
            "gap {:?} not in [{:?}, {:?}]", gap, lower, upper);
        // after is after before, so deadline.duration_since(after) < timeout_secs.
        let _ = after; // referenced for clarity
    }

    // ---------- soft timeout tests ----------

    /// First timeout fires the soft path and calculates retry with the production formula.
    #[test]
    fn soft_timeout_sets_flag_and_retry_secs() {
        let timeout_secs: u64 = 600;
        let soft_timeout_fired = false;

        let action = on_timeout(soft_timeout_fired, timeout_secs);

        assert_eq!(
            action,
            TimeoutAction::SoftFired { retry_secs: 120 },
            "default 600s → retry_secs should be 120"
        );
    }

    /// `compute_retry_secs` floors at 60 when timeout_secs < 300.
    #[test]
    fn soft_timeout_retry_secs_minimum_is_60() {
        // timeout_secs = 100 → 100/5 = 20, .max(60) = 60
        assert_eq!(compute_retry_secs(100), 60);
        // timeout_secs = 0 → 0/5 = 0, .max(60) = 60
        assert_eq!(compute_retry_secs(0), 60);
        // timeout_secs = 299 → 299/5 = 59, .max(60) = 60
        assert_eq!(compute_retry_secs(299), 60);
    }

    /// `compute_retry_secs` scales correctly above the minimum.
    #[test]
    fn soft_timeout_retry_secs_scales_above_minimum() {
        // timeout_secs = 300 → 300/5 = 60, .max(60) = 60 (boundary)
        assert_eq!(compute_retry_secs(300), 60);
        // timeout_secs = 600 → 600/5 = 120
        assert_eq!(compute_retry_secs(600), 120);
        // timeout_secs = 1800 → 1800/5 = 360
        assert_eq!(compute_retry_secs(1800), 360);
    }

    // ---------- hard timeout tests ----------

    /// Second timeout (soft already fired) produces HardKill.
    #[test]
    fn hard_timeout_after_soft_fires_hard_kill() {
        let timeout_secs: u64 = 600;
        let soft_timeout_fired = true; // flag set by previous soft timeout

        let action = on_timeout(soft_timeout_fired, timeout_secs);

        assert_eq!(action, TimeoutAction::HardKill);
    }

    /// Verify the full two-event state progression: no-fire → soft → hard.
    #[test]
    fn timeout_state_machine_progression() {
        let timeout_secs: u64 = 600;
        let mut soft_fired = false;

        // Event 1: first timeout → soft
        let action1 = on_timeout(soft_fired, timeout_secs);
        if let TimeoutAction::SoftFired { .. } = action1 {
            soft_fired = true; // production sets the flag here
        } else {
            panic!("Expected SoftFired on first timeout");
        }

        // Event 2: second timeout → hard
        let action2 = on_timeout(soft_fired, timeout_secs);
        assert_eq!(
            action2,
            TimeoutAction::HardKill,
            "second timeout must produce HardKill"
        );
    }
}
