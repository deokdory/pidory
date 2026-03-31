use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use poise::serenity_prelude::{ChannelId, MessageId};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command, ChildStdin};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use std::process::Stdio;

use crate::config::ClaudeConfig;
use crate::error::PidoryError;
use super::parser::{parse_line, StreamEvent};

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

    pub async fn get_or_create(
        &self,
        thread_id: &str,
        project_path: &str,
        session_id: Option<&str>,
        disallowed_tools: &[String],
    ) -> Result<(), PidoryError> {
        let mut sessions = self.sessions.lock().await;

        if sessions.contains_key(thread_id) {
            return Ok(());
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

        // Combined worker task: reads queue, writes stdin, reads stdout until result, streams events
        let queue_size_for_worker = Arc::clone(&queue_size);
        let timeout_secs = self.config.subprocess_timeout_secs;
        let sessions_clone = Arc::clone(&self.sessions);
        let thread_id_for_worker = thread_id.to_string();
        let worker_task = tokio::spawn(async move {
            let mut stdin = stdin;
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut queue_rx = queue_rx;

            while let Some(msg) = queue_rx.recv().await {
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
                let timeout_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
                loop {
                    line.clear();
                    tokio::select! {
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
                                        break;
                                    }
                                    let _ = stdin.flush().await;
                                    // m.event_tx는 None이므로 drop됨
                                    // 이벤트는 계속 원래 event_tx로 감
                                }
                                None => {
                                    // queue closed (kill_session)
                                    break;
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
                                    break;
                                }
                                Ok(Ok(_)) => {
                                    let trimmed = line.trim_end();
                                    if trimmed.is_empty() {
                                        continue;
                                    }
                                    match parse_line(trimmed) {
                                        Ok(event) => {
                                            let is_result = event.is_result();
                                            let _ = event_tx.send(event).await;
                                            if is_result {
                                                break; // turn complete
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
                                    break;
                                }
                                Err(_) => {
                                    tracing::error!("Turn timeout for thread {}", thread_id_for_worker);
                                    break;
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
            },
        );

        Ok(())
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
}
