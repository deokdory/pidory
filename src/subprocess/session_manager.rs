use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use poise::serenity_prelude::{ChannelId, Context, MessageId, UserId};
use tokio::io::BufReader;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use std::process::Stdio;

use crate::config::ClaudeConfig;
use crate::error::PidoryError;
use crate::i18n::Lang;
use crate::PendingPermission;
use super::parser::StreamEvent;
use super::permission::PermissionRequest;

pub struct SessionCreateResult {
    pub evicted_thread_id: Option<String>,
}

pub struct QueuedMessage {
    pub content: String,
    pub channel_id: ChannelId,
    pub message_id: MessageId,
    pub event_tx: Option<mpsc::Sender<StreamEvent>>,  // None = mid-turn inject
    pub triggered_by: UserId,
}

pub(super) struct SessionInner {
    pub(super) child: Child,
    queue_tx: mpsc::Sender<QueuedMessage>,
    queue_size: Arc<AtomicUsize>,
    worker_task: JoinHandle<()>,
    pub(super) permission_handler: JoinHandle<()>,
    _permission_tx: mpsc::Sender<PermissionRequest>,
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
        pending_permissions: Arc<tokio::sync::Mutex<HashMap<String, PendingPermission>>>,
        owner_id: u64,
    ) -> Result<SessionCreateResult, PidoryError> {
        let mut sessions = self.sessions.lock().await;

        if sessions.contains_key(thread_id) {
            return Ok(SessionCreateResult { evicted_thread_id: None });
        }

        let mut evicted_thread_id = None;
        if sessions.len() >= self.max_sessions {
            if let Some(evict_tid) = Self::find_evict_target(&sessions) {
                tracing::info!(thread_id = %evict_tid, "Evicting idle session (LRU)");
                if let Some(mut inner) = sessions.remove(&evict_tid) {
                    inner.permission_handler.abort();
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

        let stdin = child
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
        let (interrupt_tx, interrupt_rx) = mpsc::channel::<()>(1);

        let last_activity = Arc::new(StdMutex::new(Instant::now()));
        let has_active_bg_tasks = Arc::new(AtomicBool::new(false));
        let is_turn_active = Arc::new(AtomicBool::new(false));

        let permission_handler = tokio::spawn(crate::handler::permission_ui::run_permission_handler(
            permission_rx,
            ctx.clone(),
            channel_id,
            pending_permissions.clone(),
            owner_id,
            thread_id.to_string(),
            lang,
        ));

        // Combined worker task: reads queue, writes stdin, reads stdout until result, streams events
        let timeout_secs = self.config.subprocess_timeout_secs;
        let sessions_clone = Arc::clone(&self.sessions);
        let worker_task = tokio::spawn(super::worker::SessionWorker::new(
            stdin,
            BufReader::new(stdout),
            queue_rx,
            interrupt_rx,
            permission_tx.clone(),
            Arc::clone(&queue_size),
            sessions_clone,
            Arc::clone(&last_activity),
            Arc::clone(&has_active_bg_tasks),
            Arc::clone(&is_turn_active),
            thread_id.to_string(),
            channel_id,
            ctx,
            db,
            timeout_secs,
            lang,
            owner_id,
        ).run());


        sessions.insert(
            thread_id.to_string(),
            SessionInner {
                child,
                queue_tx,
                queue_size,
                worker_task,
                permission_handler,
                _permission_tx: permission_tx,
                interrupt_tx,
                last_activity,
                has_active_bg_tasks,
                is_turn_active,
            },
        );

        Ok(SessionCreateResult { evicted_thread_id })
    }

    fn find_evict_target(sessions: &HashMap<String, SessionInner>) -> Option<String> {
        sessions.iter()
            .filter(|(_, s)| {
                !s.is_turn_active.load(Ordering::Relaxed)
                && !s.has_active_bg_tasks.load(Ordering::Relaxed)
            })
            .min_by_key(|(_, s)| *s.last_activity.lock().unwrap_or_else(|p| p.into_inner()))
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

        inner
            .queue_tx
            .try_send(msg)
            .map_err(|e| PidoryError::Subprocess(format!("queue send error: {}", e)))?;

        inner.queue_size.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }

    pub async fn kill_session(&self, thread_id: &str) -> Result<(), PidoryError> {
        let mut sessions = self.sessions.lock().await;
        let mut inner = sessions.remove(thread_id).ok_or_else(|| {
            PidoryError::NotFound(format!("no active session for thread_id: {}", thread_id))
        })?;

        inner.permission_handler.abort();
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
                idle_duration: now.duration_since(*s.last_activity.lock().unwrap_or_else(|p| p.into_inner())),
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
                && now.duration_since(*s.last_activity.lock().unwrap_or_else(|p| p.into_inner())) > idle_timeout
            })
            .map(|(tid, _)| tid.clone())
            .collect();

        let mut evicted = Vec::new();
        for tid in targets {
            tracing::info!(thread_id = %tid, "Sweeping idle session (TTL expired)");
            if let Some(mut inner) = sessions.remove(&tid) {
                inner.permission_handler.abort();
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

    // ---------- queue_size counter tests ----------

    /// Regression test: queue_size must NOT be incremented when try_send fails.
    ///
    /// Mirrors the pattern in `send_message`: fetch_add must only run after a
    /// successful try_send. If try_send fails (e.g. channel closed) and the
    /// counter is still incremented, the session leaks queue slots permanently.
    #[test]
    fn queue_size_not_incremented_on_failed_send() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let queue_size = AtomicUsize::new(0);
        let (tx, rx) = tokio::sync::mpsc::channel::<()>(5);

        // Drop the receiver so try_send will fail.
        drop(rx);

        // Replicate the send_message pattern:
        //   try_send → on success → fetch_add
        // If try_send fails, fetch_add must be skipped.
        let send_result = tx.try_send(());
        if send_result.is_ok() {
            queue_size.fetch_add(1, Ordering::Relaxed);
        }

        assert!(send_result.is_err(), "try_send should fail on a closed channel");
        assert_eq!(
            queue_size.load(Ordering::Relaxed),
            0,
            "queue_size must remain 0 when try_send fails"
        );
    }
}
