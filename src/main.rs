mod commands;
mod config;
mod db;
mod error;
mod handler;
mod i18n;
mod ratelimit;
mod release;
mod subprocess;

use std::collections::{HashMap, HashSet};
use std::time::Instant;
use commands::skill::load_skill_descriptions;
use std::sync::Arc;

use poise::serenity_prelude as serenity;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, oneshot, watch};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

use config::Config;
use error::PidoryError;
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
}

/// Tracks a multi-question AskUserQuestion group.
/// Each sub-question gets its own PendingPermission keyed by `{request_id}__q{idx}`.
/// When all answers are collected, the combined answer is sent via `response_tx`.
pub struct PendingQuestionGroup {
    pub response_tx: oneshot::Sender<PermissionDecision>,
    pub input: serde_json::Value,
    pub answers: HashMap<String, String>,
    pub total: usize,
    pub thread_id: String,
    pub triggered_by: serenity::UserId,
}

pub struct Data {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub sessions: Arc<SessionManager>,
    pub pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    pub pending_question_groups: Arc<Mutex<HashMap<String, PendingQuestionGroup>>>,
    pub pending_resets: Arc<Mutex<HashMap<String, handler::reset_ui::PendingReset>>>,
    pub session_skills: Arc<Mutex<HashMap<String, Vec<String>>>>,
    pub needs_context: Arc<Mutex<HashSet<String>>>,
    pub archived_threads: Arc<Mutex<HashSet<String>>>,
    pub turn_initiators: Arc<Mutex<HashMap<String, serenity::UserId>>>,
    pub turn_participants: Arc<Mutex<HashMap<String, HashSet<serenity::UserId>>>>,
    pub skill_descriptions: HashMap<String, String>,
    /// thread_id → 마지막으로 사용된 tool name
    pub last_tool_name: Arc<Mutex<HashMap<String, String>>>,
    /// thread_id → 마지막 kick 시각
    pub kick_cooldowns: Arc<Mutex<HashMap<String, Instant>>>,
    /// kick 후 interrupt 대기 중인 thread_id 집합 (자연 완료 시 제거됨)
    pub kick_pending: Arc<Mutex<HashSet<String>>>,
    /// Event handler가 fresh Context를 background task에 전달하는 채널.
    /// Shard reconnect 후에도 최신 ShardMessenger를 사용할 수 있게 해준다.
    pub ctx_watch: watch::Sender<serenity::Context>,
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
            Box::pin(async move {
                // guild-only 커맨드 등록
                poise::builtins::register_in_guild(ctx, &framework.options().commands, guild_id)
                    .await
                    .map_err(|e| PidoryError::Discord(Box::new(e)))?;

                let db = db::init_pool(&config.database.path).await?;

                info!("Database initialized");

                // orphan 세션 정리
                let reset_count = db::repository::reset_running_sessions(&db).await?;
                if reset_count > 0 {
                    info!("Reset {} orphaned running sessions", reset_count);
                }

                let (ratelimit_tx, _) = tokio::sync::watch::channel(crate::ratelimit::RateLimitInfo::default());

                let (session_count_tx, _session_count_rx) = watch::channel(0usize);

                let sessions = Arc::new(SessionManager::new(
                    Arc::new(config.claude.clone()),
                    config.claude.max_sessions,
                    ratelimit_tx.clone(),
                    session_count_tx.clone(),
                ));

                let pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>> = Arc::new(Mutex::new(HashMap::new()));
                let pending_question_groups: Arc<Mutex<HashMap<String, PendingQuestionGroup>>> = Arc::new(Mutex::new(HashMap::new()));
                let pending_resets: Arc<Mutex<HashMap<String, handler::reset_ui::PendingReset>>> = Arc::new(Mutex::new(HashMap::new()));
                let session_skills = Arc::new(Mutex::new(HashMap::new()));
                let turn_initiators: Arc<Mutex<HashMap<String, serenity::UserId>>> = Arc::new(Mutex::new(HashMap::new()));
                let turn_participants: Arc<Mutex<HashMap<String, HashSet<serenity::UserId>>>> = Arc::new(Mutex::new(HashMap::new()));
                let skill_descriptions = load_skill_descriptions();

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
                if config.release.enabled {
                    if let Some(channel_id) = config.discord.notification_channel_id
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
                }

                let last_tool_name: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
                let kick_cooldowns: Arc<Mutex<HashMap<String, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
                let kick_pending: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
                let needs_context = Arc::new(Mutex::new(HashSet::new()));

                // Idle session TTL sweep
                {
                    let sessions = Arc::clone(&sessions);
                    let idle_timeout = std::time::Duration::from_secs(config.claude.idle_timeout_secs);
                    let pending_permissions = Arc::clone(&pending_permissions);
                    let pending_question_groups = Arc::clone(&pending_question_groups);
                    let session_skills = Arc::clone(&session_skills);
                    let needs_context = Arc::clone(&needs_context);
                    let turn_initiators = Arc::clone(&turn_initiators);
                    let turn_participants = Arc::clone(&turn_participants);
                    let last_tool_name = Arc::clone(&last_tool_name);
                    let kick_cooldowns = Arc::clone(&kick_cooldowns);
                    let kick_pending = Arc::clone(&kick_pending);
                    let db_clone = db.clone();
                    let lang = config.language;
                    let mut ctx_rx = ctx_tx.subscribe();
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
                                        pending_permissions.lock().await.retain(|_, p| p.thread_id != *tid);
                                        pending_question_groups.lock().await.retain(|_, g| g.thread_id != *tid);
                                        session_skills.lock().await.remove(tid);
                                        needs_context.lock().await.remove(tid);
                                        turn_initiators.lock().await.remove(tid);
                                        turn_participants.lock().await.remove(tid);
                                        last_tool_name.lock().await.remove(tid);
                                        kick_cooldowns.lock().await.remove(tid);
                                        kick_pending.lock().await.remove(tid);
                                        if let Err(e) = db::repository::update_session_status(&db_clone, tid, "idle").await {
                                            tracing::warn!("Failed to update session status for TTL sweep thread {}: {}", tid, e);
                                        }
                                        if let Ok(channel_id) = tid.parse::<u64>() {
                                            let ctx = ctx_rx.borrow().clone();
                                            poise::serenity_prelude::ChannelId::new(channel_id)
                                                .say(&ctx, format!("-# ⏰ {}", lang.session_idle_cleaned()))
                                                .await
                                                .ok();
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


                Ok(Data {
                    config,
                    db,
                    sessions,
                    pending_permissions,
                    pending_question_groups,
                    pending_resets,
                    session_skills,
                    needs_context,
                    archived_threads: Arc::new(Mutex::new(HashSet::new())),
                    turn_initiators,
                    turn_participants,
                    skill_descriptions,
                    last_tool_name,
                    kick_cooldowns,
                    kick_pending,
                    ctx_watch: ctx_tx,
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
