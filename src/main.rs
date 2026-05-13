#![warn(clippy::await_holding_lock)]

mod claude_settings;
mod commands;
mod config;
mod db;
mod error;
mod handler;
mod i18n;
mod ratelimit;
mod release;
mod subprocess;
mod update;

use std::collections::{HashMap, HashSet};
use commands::skill::load_skill_descriptions;
use std::sync::Arc;

use poise::serenity_prelude as serenity;
use sqlx::PgPool;
use tokio::sync::{Mutex, oneshot, watch};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

use config::Config;
use error::PidoryError;
use handler::dispatch_locks::ThreadDispatchLocks;
use handler::session_state::SessionState;
use subprocess::permission::PermissionDecision;
use subprocess::session_manager::SessionManager;

type Error = PidoryError;
type Context<'a> = poise::Context<'a, Data, Error>;

pub struct PendingPermission {
    pub response_tx: oneshot::Sender<PermissionDecision>,
    pub tool_name: String,
    pub message_id: serenity::MessageId,
    pub thread_id: String,
    pub triggered_by: serenity::UserId,
    pub input: Option<serde_json::Value>,
    /// User-selected scope override for AlwaysAllow. None = use default_scope().
    pub scope_override: Option<claude_settings::rule::Scope>,
    /// Claude CLI 가 control_request 에 포함한 decision_reason. Level 2 UI 에 보존.
    pub decision_reason: Option<String>,
    /// Worker session's project directory (path_safety 검사용).
    pub cwd: std::path::PathBuf,
    /// Resolved settings additionalDirectories (path_safety 검사용).
    pub additional_dirs: std::sync::Arc<Vec<std::path::PathBuf>>,
    /// tool input에서 추출한 file_path (Edit/Write/Read 등). 없으면 None.
    pub file_path: Option<String>,
}

/// Tracks a multi-question AskUserQuestion group.
/// Each sub-question gets its own PendingPermission keyed by `{request_id}__q{idx}`.
/// When all answers are collected, the combined answer is sent via `response_tx`.
///
/// `answered` tracks which sub-question indices have been answered. We can't use
/// `answers.len() == total` for completion because `answers` is keyed by question
/// text (Claude CLI ≥ 2.1.121 looks up answers by `question.question`). If two
/// questions share the same text — or `resolve_question_text` falls back to `""`
/// — the second insert overwrites the first, and `len()` would never reach `total`.
/// `answered` is keyed by sub-question index so it's collision-free. See PR #275.
pub struct PendingQuestionGroup {
    pub response_tx: oneshot::Sender<PermissionDecision>,
    pub input: serde_json::Value,
    pub answers: HashMap<String, String>,
    pub answered: HashSet<usize>,
    pub total: usize,
    pub thread_id: String,
    pub triggered_by: serenity::UserId,
}

pub struct Data {
    pub config: Arc<Config>,
    pub db: PgPool,
    pub sessions: Arc<SessionManager>,
    pub pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    pub pending_question_groups: Arc<Mutex<HashMap<String, PendingQuestionGroup>>>,
    pub pending_resets: Arc<Mutex<HashMap<String, handler::reset_ui::PendingReset>>>,
    pub dispatch_locks: Arc<ThreadDispatchLocks>,
    pub session_states: Arc<Mutex<HashMap<String, SessionState>>>,
    pub skill_descriptions: HashMap<String, String>,
    pub agent_descriptions: HashMap<String, String>,
    /// Event handler가 fresh Context를 background task에 전달하는 채널.
    /// Shard reconnect 후에도 최신 ShardMessenger를 사용할 수 있게 해준다.
    pub ctx_watch: watch::Sender<serenity::Context>,
    /// AllowAlways 성공 후 다음 user message 도착 시 subprocess --resume 재시작 예약.
    /// Claude CLI 가 settings.local.json 을 핫 리로드하지 않으므로
    /// 새 subprocess 가 settings 를 다시 읽어 룰 매칭이 올바르게 동작한다.
    pub pending_session_restart: Arc<Mutex<HashSet<String>>>,
}

#[tokio::main]
async fn main() -> Result<(), PidoryError> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::load_from_env()?;
    let config = Arc::new(config);

    let token = std::env::var(&config.discord.token_env)
        .map_err(|_| PidoryError::Config(format!("{} environment variable not set", config.discord.token_env)))?;

    let guild_id = serenity::GuildId::new(config.discord.guild_id);
    let owner_id = serenity::UserId::new(config.discord.owner_id);

    info!("Starting pidory v{}...", env!("CARGO_PKG_VERSION"));

    // Self-check: stale lock 정리 + 업데이트 마커 확인 → 필요 시 자동 롤백
    let worktree_opt = match update::worktree::detect_worktree() {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("Worktree detection failed: {:?}. Self-check skipped.", e);
            None
        }
    };

    // check_and_recover가 마커를 수정/삭제하기 전에 기록 — watchdog gate 용도.
    let had_pending_marker = worktree_opt
        .as_ref()
        .map(|w| w.join("target").join("release").join(".update-pending").exists())
        .unwrap_or(false);

    if let Some(worktree) = &worktree_opt {
        if let Err(e) = update::lock::cleanup_stale(worktree) {
            tracing::warn!("cleanup_stale failed: {:?}", e);
        }
        match update::marker::check_and_recover(worktree) {
            update::marker::RecoveryAction::Normal => {}
            update::marker::RecoveryAction::Rolling { from, to, attempt } => {
                tracing::warn!("Rolling back: from={} to={} attempt={}", from, to, attempt);
                let backup_dir = std::path::Path::new(&config.database.path)
                    .parent()
                    .unwrap_or(std::path::Path::new("."));
                let backup_path = backup_dir.join("pidory-backup.sql");
                let database_url = match std::env::var("DATABASE_URL") {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::error!("DATABASE_URL missing during rollback — DB restore skipped");
                        String::new()
                    }
                };
                let mut restore_failed = false;
                if let Err(e) = update::backup::restore_binary(worktree) {
                    tracing::error!("restore_binary failed: {:?}", e);
                    restore_failed = true;
                }
                if !database_url.is_empty() {
                    if let Err(e) = update::backup::restore_db(&database_url, &backup_path) {
                        tracing::error!("restore_db failed: {:?}", e);
                        restore_failed = true;
                    }
                } else {
                    restore_failed = true;
                }
                let rollback_marker = worktree.join("target").join("release").join(".update-rolled-back");
                let rollback_info = serde_json::json!({
                    "from": from,
                    "to": to,
                    "attempt": attempt,
                    "restore_failed": restore_failed,
                });
                let _ = std::fs::write(&rollback_marker, rollback_info.to_string());

                if restore_failed {
                    // 복원 자체가 실패한 상태에서 재시작을 예약하면, attempts 한계에 도달한 뒤
                    // check_and_recover가 marker를 삭제하고 Normal 경로로 떨어져
                    // 망가진 새 바이너리가 steady state로 자리잡는다.
                    // 대신 비정상 종료하여 systemd의 Restart=on-failure에 의존하고,
                    // 마커를 그대로 두어 다음 부팅에서도 롤백 시도를 계속한다.
                    tracing::error!(
                        "rollback restore failed — exiting 1 without scheduling, marker preserved"
                    );
                    std::process::exit(1);
                }

                if let Err(e) = update::restart::schedule_restart() {
                    tracing::error!("rollback schedule_restart failed: {:?}", e);
                    std::process::exit(1);
                }
                std::process::exit(0);
            }
        }
    }

    // ready watchdog spawn — pending 마커가 존재했을 때만 arm.
    // 일반 부팅(업데이트 없음)에서 watchdog이 Discord 장애 등으로 60초 내
    // 준비 신호를 못 받으면 불필요한 강제 재시작 루프가 시작되기 때문이다.
    let ready_tx_cell: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>> = {
        if had_pending_marker {
            if let Some(worktree) = worktree_opt.clone() {
                let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
                tokio::spawn(update::marker::ready_watchdog(worktree, ready_rx));
                Arc::new(std::sync::Mutex::new(Some(ready_tx)))
            } else {
                Arc::new(std::sync::Mutex::new(None))
            }
        } else {
            Arc::new(std::sync::Mutex::new(None))
        }
    };

    let config_clone = config.clone();
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all_commands(),
            owners: HashSet::from([owner_id]),
            event_handler: |ctx, event, _framework, data| {
                Box::pin(handler::message::handle_event(ctx, event, data))
            },
            ..Default::default()
        })
        .setup(move |ctx, _ready, framework| {
            let config = config_clone.clone();
            let ready_tx_cell = ready_tx_cell.clone();
            let worktree_opt = worktree_opt.clone();
            Box::pin(async move {
                // Discord gateway 연결됨 → ready signal 송신 → watchdog 정상 종료
                if let Some(tx) = ready_tx_cell.lock().unwrap().take() {
                    let _ = tx.send(());
                }

                // 롤백 알림 (있으면)
                if let Some(worktree) = &worktree_opt {
                    let rollback_marker = worktree.join("target").join("release").join(".update-rolled-back");
                    if rollback_marker.exists() {
                        if let Ok(contents) = std::fs::read_to_string(&rollback_marker)
                            && let Some(channel_id) = config.discord.notification_channel_id
                        {
                            let msg = format!(
                                "⚠️ 업데이트 후 부팅 실패로 자동 롤백됨. 상세: {}",
                                contents
                            );
                            let _ = poise::serenity_prelude::ChannelId::new(channel_id)
                                .say(&ctx, msg)
                                .await;
                        }
                        let _ = std::fs::remove_file(&rollback_marker);
                    }
                }

                // guild-only 커맨드 등록
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id)
                    .await
                    .map_err(|e| PidoryError::Discord(Box::new(e)))?;

                let database_url = std::env::var("DATABASE_URL")
                    .map_err(|_| PidoryError::Config("DATABASE_URL environment variable not set".to_string()))?;
                let db = db::init_pool(&database_url).await?;

                info!("Database initialized");

                // orphan 세션 정리
                let reset_count = db::repository::reset_running_sessions(&db).await?;
                if reset_count > 0 {
                    info!("Reset {} orphaned running sessions", reset_count);
                }

                // default_scope cache 부팅 초기화
                db::repository::load_default_scope_from_db(&db, config.discord.owner_id as i64).await;

                let (ratelimit_tx, _) = tokio::sync::watch::channel(crate::ratelimit::RateLimitInfo::default());

                let (session_count_tx, _session_count_rx) = watch::channel(0usize);

                let sessions = Arc::new(SessionManager::new(
                    Arc::new(config.claude.clone()),
                    config.footer.clone(),
                    config.claude.max_sessions,
                    ratelimit_tx.clone(),
                    session_count_tx.clone(),
                ));

                let pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>> = Arc::new(Mutex::new(HashMap::new()));
                let pending_question_groups: Arc<Mutex<HashMap<String, PendingQuestionGroup>>> = Arc::new(Mutex::new(HashMap::new()));
                let pending_resets: Arc<Mutex<HashMap<String, handler::reset_ui::PendingReset>>> = Arc::new(Mutex::new(HashMap::new()));
                let skill_descriptions = load_skill_descriptions();
                let agent_descriptions = commands::agent::load_global_agent_descriptions();

                // watch channel: event handler → background task로 fresh Context 전달
                // shard reconnect 후에도 최신 ShardMessenger 사용 가능
                let (ctx_tx, ctx_rx) = watch::channel(ctx.clone());

                // Rate limit monitor (changed() 기반 반응형)
                {
                    let mut ctx_rx = ctx_rx;
                    let mut ratelimit_rx = ratelimit_tx.subscribe();
                    let notification_channel = config.discord.notification_channel_id
                        .map(poise::serenity_prelude::ChannelId::new);
                    tokio::spawn(async move {
                        let mut monitor = crate::ratelimit::RateLimitMonitor::new();
                        tracing::info!("Rate limit monitor started (reactive, changed() based)");
                        loop {
                            tokio::select! {
                                result = ratelimit_rx.changed() => {
                                    if result.is_err() { break; }
                                    let info = ratelimit_rx.borrow_and_update().clone();
                                    if info.updated_at == 0 {
                                        continue;
                                    }
                                    if let Some(channel_id) = notification_channel {
                                        let fresh_ctx = ctx_rx.borrow().clone();
                                        monitor.notify_if_changed(&info, &fresh_ctx, channel_id).await;
                                    }
                                }
                                result = ctx_rx.changed() => {
                                    if result.is_err() { break; }
                                    tracing::debug!("Rate limit monitor: context refreshed");
                                }
                            }
                        }
                    });
                }

                // Release checker
                if config.release.enabled
                    && let Some(channel_id) = config.discord.notification_channel_id
                        .map(poise::serenity_prelude::ChannelId::new)
                {
                    let repo = config.release.repo.clone();
                    let last_tag_file = config.release.last_tag_file.clone();
                    let interval_secs = config.release.check_interval_secs;
                    let token = config.release.token_env.as_ref()
                        .and_then(|env_name| std::env::var(env_name).ok());
                    let lang = config.language;
                    let mut ctx_rx = ctx_tx.subscribe();
                    tokio::spawn(async move {
                        let checker = crate::release::ReleaseChecker::new(repo, last_tag_file, token);
                        let mut interval = tokio::time::interval(
                            std::time::Duration::from_secs(interval_secs),
                        );
                        tracing::info!("Release checker started (interval: {interval_secs}s)");
                        loop {
                            tokio::select! {
                                _ = interval.tick() => {
                                    let fresh_ctx = ctx_rx.borrow().clone();
                                    checker.check_and_notify(&fresh_ctx, channel_id, lang).await;
                                }
                                result = ctx_rx.changed() => {
                                    if result.is_err() { break; }
                                    tracing::debug!("Release checker: context refreshed");
                                }
                            }
                        }
                    });
                }

                let session_states: Arc<Mutex<HashMap<String, SessionState>>> = Arc::new(Mutex::new(HashMap::new()));
                let dispatch_locks: Arc<ThreadDispatchLocks> = Arc::new(ThreadDispatchLocks::new());

                // Idle session TTL sweep
                {
                    let sessions = Arc::clone(&sessions);
                    let idle_timeout = std::time::Duration::from_secs(config.claude.idle_timeout_secs);
                    let db_clone = db.clone();
                    let mut ctx_rx = ctx_tx.subscribe();
                    // pending_recalls placeholder — TTL sweep은 recall 없음
                    let placeholder_recalls = Arc::new(tokio::sync::Mutex::new(
                        std::collections::HashMap::<serenity::MessageId, (String, Arc<std::sync::atomic::AtomicBool>)>::new(),
                    ));
                    let cleanup_handles = subprocess::supervisor::SessionCleanupHandles {
                        pending_permissions: Arc::clone(&pending_permissions),
                        pending_question_groups: Arc::clone(&pending_question_groups),
                        pending_resets: Arc::clone(&pending_resets),
                        session_states: Arc::clone(&session_states),
                        pending_recalls: placeholder_recalls,
                        dispatch_locks: Arc::clone(&dispatch_locks),
                    };
                    tokio::spawn(async move {
                        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
                        loop {
                            tokio::select! {
                                _ = interval.tick() => {
                                    let evicted = sessions.sweep_idle_sessions(idle_timeout).await;
                                    if evicted.is_empty() {
                                        continue;
                                    }
                                    tracing::info!("TTL sweep: evicted {} sessions", evicted.len());
                                    for tid in &evicted {
                                        let ctx = ctx_rx.borrow().clone();
                                        crate::handler::cleanup::cleanup_session_state_from_handles(
                                            &cleanup_handles,
                                            tid,
                                            &ctx,
                                        )
                                        .await;
                                        if let Err(e) = db::repository::update_session_status(&db_clone, tid, "idle").await {
                                            tracing::warn!("Failed to update session status for TTL sweep thread {}: {}", tid, e);
                                        }
                                    }
                                }
                                result = ctx_rx.changed() => {
                                    if result.is_err() { break; }
                                    tracing::debug!("TTL sweep: context refreshed");
                                }
                            }
                        }
                    });
                }

                // Session presence monitor (changed() 기반 반응형)
                {
                    let max_sessions = config.claude.max_sessions;
                    let mut session_count_rx = session_count_tx.subscribe();
                    let mut ctx_rx = ctx_tx.subscribe();
                    tokio::spawn(async move {
                        tracing::info!("Session presence monitor started");
                        {
                            let count = *session_count_rx.borrow();
                            let ctx = ctx_rx.borrow().clone();
                            ctx.set_activity(Some(
                                serenity::gateway::ActivityData::custom(
                                    format!("Sessions: {count}/{max_sessions}")
                                )
                            ));
                        }
                        loop {
                            tokio::select! {
                                result = session_count_rx.changed() => {
                                    if result.is_err() { break; }
                                    let count = *session_count_rx.borrow_and_update();
                                    let ctx = ctx_rx.borrow().clone();
                                    ctx.set_activity(Some(
                                        serenity::gateway::ActivityData::custom(
                                            format!("Sessions: {count}/{max_sessions}")
                                        )
                                    ));
                                }
                                result = ctx_rx.changed() => {
                                    if result.is_err() { break; }
                                    let count = *session_count_rx.borrow();
                                    let ctx = ctx_rx.borrow_and_update().clone();
                                    ctx.set_activity(Some(
                                        serenity::gateway::ActivityData::custom(
                                            format!("Sessions: {count}/{max_sessions}")
                                        )
                                    ));
                                    tracing::debug!("Session presence: context refreshed, activity re-applied");
                                }
                            }
                        }
                    });
                }

                Ok(Data {
                    config,
                    db,
                    sessions,
                    pending_permissions,
                    pending_question_groups,
                    pending_resets,
                    dispatch_locks,
                    session_states,
                    skill_descriptions,
                    agent_descriptions,
                    ctx_watch: ctx_tx,
                    pending_session_restart: Arc::new(Mutex::new(HashSet::new())),
                })
            })
        })
        .build();

    let intents = serenity::GatewayIntents::GUILD_MESSAGES
        | serenity::GatewayIntents::MESSAGE_CONTENT
        | serenity::GatewayIntents::GUILDS;

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .map_err(|e| PidoryError::Discord(Box::new(e)))?;

    client.start().await.map_err(|e| PidoryError::Discord(Box::new(e)))?;

    Ok(())
}
