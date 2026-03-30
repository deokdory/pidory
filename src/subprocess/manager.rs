use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use std::process::Stdio;

use crate::config::ClaudeConfig;
use crate::error::PidoryError;

use super::parser::{parse_line, StreamEvent};

pub struct SubprocessManager {
    active: Arc<Mutex<HashMap<String, Child>>>,
    config: Arc<ClaudeConfig>,
    max_concurrent: usize,
}

impl SubprocessManager {
    pub fn new(config: Arc<ClaudeConfig>) -> Self {
        let max_concurrent = config.max_concurrent;
        Self {
            active: Arc::new(Mutex::new(HashMap::new())),
            config,
            max_concurrent,
        }
    }

    pub async fn spawn(
        &self,
        thread_id: &str,
        project_path: &str,
        prompt: &str,
        session_id: Option<&str>,
        disallowed_tools: &[String],
    ) -> Result<Vec<StreamEvent>, PidoryError> {
        {
            let active = self.active.lock().await;
            if active.len() >= self.max_concurrent {
                return Err(PidoryError::Subprocess(format!(
                    "max concurrent subprocesses reached ({})",
                    self.max_concurrent
                )));
            }
        }

        let mut cmd = Command::new(&self.config.binary_path);
        cmd.arg("-p")
            .arg(prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose");

        if let Some(sid) = session_id {
            cmd.arg("--resume").arg(sid);
        }

        if !disallowed_tools.is_empty() {
            cmd.arg("--disallowedTools").arg(disallowed_tools.join(","));
        }

        cmd.current_dir(project_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| PidoryError::Subprocess(format!("failed to spawn process: {}", e)))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PidoryError::Subprocess("no stdout handle".to_string()))?;

        {
            let mut active = self.active.lock().await;
            active.insert(thread_id.to_string(), child);
        }

        let active_clone = Arc::clone(&self.active);
        let thread_id_owned = thread_id.to_string();
        let timeout_secs = self.config.subprocess_timeout_secs;

        let collect_future = async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut events = Vec::new();

            loop {
                line.clear();
                let n = reader
                    .read_line(&mut line)
                    .await
                    .map_err(|e| PidoryError::Subprocess(format!("read error: {}", e)))?;
                if n == 0 {
                    break;
                }
                let event = parse_line(line.trim_end())?;
                events.push(event);
            }

            Ok::<Vec<StreamEvent>, PidoryError>(events)
        };

        let result =
            tokio::time::timeout(Duration::from_secs(timeout_secs), collect_future).await;

        match result {
            Ok(Ok(events)) => {
                let mut active = active_clone.lock().await;
                active.remove(&thread_id_owned);
                Ok(events)
            }
            Ok(Err(e)) => {
                let mut active = active_clone.lock().await;
                if let Some(mut child) = active.remove(&thread_id_owned) {
                    let _ = child.kill().await;
                }
                Err(e)
            }
            Err(_elapsed) => {
                let mut active = active_clone.lock().await;
                if let Some(mut child) = active.remove(&thread_id_owned) {
                    let _ = child.kill().await;
                }
                Err(PidoryError::Subprocess("timeout".to_string()))
            }
        }
    }

    pub async fn kill(&self, thread_id: &str) -> Result<(), PidoryError> {
        let mut active = self.active.lock().await;
        match active.remove(thread_id) {
            Some(mut child) => child
                .kill()
                .await
                .map_err(|e| PidoryError::Subprocess(format!("kill failed: {}", e))),
            None => Err(PidoryError::NotFound(format!(
                "no active subprocess for thread_id: {}",
                thread_id
            ))),
        }
    }

    pub async fn is_running(&self, thread_id: &str) -> bool {
        let active = self.active.lock().await;
        active.contains_key(thread_id)
    }

    pub async fn active_count(&self) -> usize {
        let active = self.active.lock().await;
        active.len()
    }
}
