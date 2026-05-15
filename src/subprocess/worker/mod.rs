use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::Instant;


use poise::serenity_prelude::{ChannelId, Context, MessageId, UserId};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{Mutex, mpsc};

use crate::config::TimestampConfig;
use crate::handler::session_state::SessionState;
use crate::i18n::Lang;
use crate::ratelimit::RateLimitInfo;
use super::background::BackgroundTaskTracker;
use super::permission::{PermissionCache, PermissionRequest};
use super::session_manager::{QueuedMessage, SessionInner};

mod io;
mod ratelimit_bridge;
mod permission_wait;
mod turn_between;
mod turn_active;
mod turn_bg;

// ─── Helpers ────────────────────────────────────────────────────────────────

use io::build_user_message_json;
use turn_between::{handle_between_turns_event, BetweenTurnsAction};
use turn_active::run_active_turn;

// ─── SessionWorker struct ───────────────────────────────────────────────────

#[allow(clippy::type_complexity)]
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
    session_states: Arc<Mutex<HashMap<String, SessionState>>>,
    // Config/Context
    thread_id: String,
    channel_id: ChannelId,
    ctx: Context,
    db: sqlx::PgPool,
    timeout_secs: u64,
    lang: Lang,
    show_context_percent: bool,
    timestamp_config: TimestampConfig,
    permission_response_timeout_secs: u64,
    // Permission path context
    pub(super) project_path: PathBuf,
    pub(super) additional_dirs: Arc<Vec<PathBuf>>,
}

impl SessionWorker {
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
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
        db: sqlx::PgPool,
        timeout_secs: u64,
        lang: Lang,
        owner_id: u64,
        show_context_percent: bool,
        timestamp_config: TimestampConfig,
        permission_response_timeout_secs: u64,
        pending_recalls: Arc<tokio::sync::Mutex<HashMap<MessageId, (String, Arc<AtomicBool>)>>>,
        ratelimit_tx: tokio::sync::watch::Sender<RateLimitInfo>,
        session_states: Arc<Mutex<HashMap<String, SessionState>>>,
        project_path: PathBuf,
        additional_dirs: Arc<Vec<PathBuf>>,
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
            session_states,
            thread_id,
            channel_id,
            ctx,
            db,
            timeout_secs,
            lang,
            show_context_percent,
            timestamp_config,
            permission_response_timeout_secs,
            project_path,
            additional_dirs,
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
            ref session_states,
            ref thread_id,
            ref channel_id,
            ref ctx,
            ref db,
            timeout_secs,
            lang,
            show_context_percent,
            ref timestamp_config,
            permission_response_timeout_secs,
            ref project_path,
            ref additional_dirs,
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
                session_states,
                thread_id,
                channel_id,
                ctx,
                db,
                lang,
                show_context_percent,
                &mut model_name,
                project_path,
                additional_dirs,
                timestamp_config,
                permission_response_timeout_secs,
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

                    let now = chrono::Utc::now();
                    let json_line = build_user_message_json(&msg.content, &msg.downloaded_files, msg.reply_context.as_ref(), msg.sender_info.as_ref(), timestamp_config, now);
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
                        project_path,
                        additional_dirs,
                        timestamp_config,
                        permission_response_timeout_secs,
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
            super::session_manager::kill_with_timeout(&mut inner.child).await;
        }
    }
}
