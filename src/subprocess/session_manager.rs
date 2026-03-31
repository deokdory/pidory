use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

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
use super::background::BackgroundTaskTracker;
use super::parser::{parse_line, StreamEvent, ContentBlock, build_control_response_allow, build_control_response_deny};
use super::permission::{PermissionCache, PermissionDecision, PermissionRequest};

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
    ) -> Result<Option<mpsc::Receiver<PermissionRequest>>, PidoryError> {
        let mut sessions = self.sessions.lock().await;

        if sessions.contains_key(thread_id) {
            return Ok(None);
        }

        if sessions.len() >= self.max_sessions {
            return Err(PidoryError::Subprocess(format!(
                "max sessions reached ({})",
                self.max_sessions
            )));
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

        // Combined worker task: reads queue, writes stdin, reads stdout until result, streams events
        let queue_size_for_worker = Arc::clone(&queue_size);
        let timeout_secs = self.config.subprocess_timeout_secs;
        let sessions_clone = Arc::clone(&self.sessions);
        let thread_id_for_worker = thread_id.to_string();
        let permission_tx_for_worker = permission_tx.clone();
        let ctx_for_worker = ctx;
        let channel_id_for_worker = channel_id;
        let db_for_worker = db;
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
                                        tracing::info!("Background task started: {} ({})", task_id, task_type);
                                        let start_msg = format!("-# 🔔 Background task started: {}", description);
                                        channel_id_for_worker.say(&ctx_for_worker, &start_msg).await.ok();
                                        continue;
                                    }
                                    Ok(StreamEvent::TaskProgress { ref task_id, ref description, .. }) => {
                                        tracker.track_progress(task_id, description);
                                        continue;
                                    }
                                    Ok(StreamEvent::TaskNotification { ref task_id, ref status, ref summary, .. }) => {
                                        tracker.track_completed(task_id);
                                        let notify_msg = if status == "completed" {
                                            format!("-# 🔔 {}", summary)
                                        } else {
                                            format!("-# ❌ {}", summary)
                                        };
                                        channel_id_for_worker.say(&ctx_for_worker, &notify_msg).await.ok();

                                        repository::update_session_status(&db_for_worker, &thread_id_for_worker, "running").await.ok();

                                        // bg mini-loop: background turn 이벤트 처리
                                        'bg_turn: loop {
                                            line.clear();
                                            tokio::select! {
                                                read_result = reader.read_line(&mut line) => {
                                                    match read_result {
                                                        Ok(0) => {
                                                            repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await.ok();
                                                            break 'bg_turn;
                                                        }
                                                        Ok(_) => {
                                                            let trimmed = line.trim_end();
                                                            if trimmed.is_empty() { continue 'bg_turn; }
                                                            match parse_line(trimmed) {
                                                                Ok(StreamEvent::Result { .. }) => {
                                                                    repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await.ok();
                                                                    break 'bg_turn;
                                                                }
                                                                Ok(StreamEvent::Assistant { ref content, .. }) => {
                                                                    for block in content {
                                                                        match block {
                                                                            ContentBlock::Text(text) if !text.trim().is_empty() => {
                                                                                let bg_text = format!("-# 🔔 [Background]\n{}", text);
                                                                                channel_id_for_worker.say(&ctx_for_worker, &bg_text).await.ok();
                                                                            }
                                                                            ContentBlock::ToolUse { name, input, .. } => {
                                                                                let formatted = formatter::format_tool_use(name, input);
                                                                                let bg_text = format!("-# 🔔 [Background]\n{}", formatted);
                                                                                channel_id_for_worker.say(&ctx_for_worker, &bg_text).await.ok();
                                                                            }
                                                                            _ => {}
                                                                        }
                                                                    }
                                                                }
                                                                Ok(StreamEvent::User { ref tool_results, .. }) => {
                                                                    for result in tool_results {
                                                                        if result.is_error {
                                                                            if let Some(formatted) = formatter::format_tool_result_with_name(result, None) {
                                                                                let bg_text = format!("-# 🔔 [Background]\n{}", formatted);
                                                                                channel_id_for_worker.say(&ctx_for_worker, &bg_text).await.ok();
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                                Ok(StreamEvent::ControlRequest { ref request_id, ref tool_name, ref input, .. }) => {
                                                                    let resp = if permission_cache.is_always_allowed(tool_name) {
                                                                        build_control_response_allow(request_id, input)
                                                                    } else {
                                                                        let deny_msg = format!(
                                                                            "-# ⚠️ [Background] Permission denied: {} (not in cache)",
                                                                            tool_name
                                                                        );
                                                                        channel_id_for_worker.say(&ctx_for_worker, &deny_msg).await.ok();
                                                                        build_control_response_deny(request_id, "Background: permission not cached")
                                                                    };
                                                                    if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                                                        tracing::error!("stdin write error (bg turn): {}", e);
                                                                        repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await.ok();
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
                                                            repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await.ok();
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
                                                                repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await.ok();
                                                                break 'bg_turn;
                                                            }
                                                            let _ = stdin.flush().await;
                                                        }
                                                        None => {
                                                            repository::update_session_status(&db_for_worker, &thread_id_for_worker, "idle").await.ok();
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
                                            let deny_msg = format!(
                                                "-# ⚠️ [Background] Permission denied: {} (not in cache)",
                                                tool_name
                                            );
                                            channel_id_for_worker.say(&ctx_for_worker, &deny_msg).await.ok();
                                            build_control_response_deny(request_id, "Background: permission not cached")
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

                // primary 메시지: result까지 stdout 읽기 + mid-turn inject 동시 처리
                let mut timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
                let mut bg_turn_active = false;
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
                                    let trimmed = line.trim_end();
                                    if trimmed.is_empty() {
                                        continue 'turn;
                                    }
                                    match parse_line(trimmed) {
                                        Ok(event) => {
                                            // Background task 이벤트: user turn 중에도 올 수 있음
                                            match &event {
                                                StreamEvent::TaskStarted { task_id, task_type, description, .. } => {
                                                    tracker.track_started(task_id, task_type, description);
                                                    let start_msg = format!("-# 🔔 Background task started: {}", description);
                                                    channel_id_for_worker.say(&ctx_for_worker, &start_msg).await.ok();
                                                    continue 'turn;
                                                }
                                                StreamEvent::TaskProgress { task_id, description, .. } => {
                                                    tracker.track_progress(task_id, description);
                                                    continue 'turn;
                                                }
                                                StreamEvent::TaskNotification { task_id, status, summary, .. } => {
                                                    tracker.track_completed(task_id);
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
                                                        continue 'turn; // bg turn 끝 — user turn은 계속
                                                    }
                                                    StreamEvent::Assistant { content, .. } => {
                                                        for block in content {
                                                            match block {
                                                                ContentBlock::Text(text) if !text.trim().is_empty() => {
                                                                    let bg_text = format!("-# 🔔 [Background]\n{}", text);
                                                                    channel_id_for_worker.say(&ctx_for_worker, &bg_text).await.ok();
                                                                }
                                                                ContentBlock::ToolUse { name, input, .. } => {
                                                                    let formatted = formatter::format_tool_use(name, input);
                                                                    let bg_text = format!("-# 🔔 [Background]\n{}", formatted);
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
                                                                if let Some(formatted) = formatter::format_tool_result_with_name(result, None) {
                                                                    let bg_text = format!("-# 🔔 [Background]\n{}", formatted);
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
                                                            let deny_msg = format!("-# ⚠️ [Background] Permission denied: {} (not in cache)", tool_name);
                                                            channel_id_for_worker.say(&ctx_for_worker, &deny_msg).await.ok();
                                                            build_control_response_deny(request_id, "Background: permission not cached")
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
                                                continue 'turn;
                                            }

                                            // 일반 이벤트 처리
                                            let is_result = event.is_result();
                                            let _ = event_tx.send(event).await;
                                            if is_result {
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
                                    tracing::error!("Turn timeout for thread {}", thread_id_for_worker);
                                    break 'turn;
                                }
                            }
                        }
                    }
                }
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
            },
        );

        Ok(Some(permission_rx))
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
}
