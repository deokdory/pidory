mod commands;
mod config;
mod db;
mod error;
mod handler;
mod i18n;
mod ratelimit;
mod subprocess;

use std::collections::{HashMap, HashSet};
use commands::skill::load_skill_descriptions;
use std::sync::Arc;

use poise::serenity_prelude as serenity;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

use config::Config;
use error::PidoryError;
use subprocess::permission::{PermissionDecision, PermissionRequest};
use subprocess::session_manager::SessionManager;

type Error = PidoryError;
type Context<'a> = poise::Context<'a, Data, Error>;

pub struct PendingPermission {
    pub response_tx: oneshot::Sender<PermissionDecision>,
    pub tool_name: String,
    pub message_id: serenity::MessageId,
}

pub struct Data {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub sessions: Arc<SessionManager>,
    pub permission_rxs: Arc<Mutex<HashMap<String, mpsc::Receiver<PermissionRequest>>>>,
    pub pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    pub session_skills: Arc<Mutex<HashMap<String, Vec<String>>>>,
    pub skill_descriptions: HashMap<String, String>,
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

    info!("Starting pidory bot...");

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

                let sessions = Arc::new(SessionManager::new(
                    Arc::new(config.claude.clone()),
                    config.claude.max_sessions,
                ));

                let permission_rxs = Arc::new(Mutex::new(HashMap::new()));
                let pending_permissions = Arc::new(Mutex::new(HashMap::new()));
                let session_skills = Arc::new(Mutex::new(HashMap::new()));
                let skill_descriptions = load_skill_descriptions();

                // watch channel: event handler → background task로 fresh Context 전달
                // shard reconnect 후에도 최신 ShardMessenger 사용 가능
                let (ctx_tx, ctx_rx) = watch::channel(ctx.clone());

                // Rate limit monitor (config.ratelimit.file_path가 Some일 때만)
                if let Some(ref file_path) = config.ratelimit.file_path {
                    let mut ctx_rx = ctx_rx;
                    let file_path = file_path.clone();
                    let interval_secs = config.ratelimit.update_interval_secs;
                    let thresholds = config.ratelimit.alert_thresholds.clone();
                    let lang = config.language;
                    let notification_channel = config.discord.notification_channel_id
                        .map(poise::serenity_prelude::ChannelId::new);
                    tokio::spawn(async move {
                        let mut monitor = crate::ratelimit::RateLimitMonitor::new(thresholds);
                        let mut interval = tokio::time::interval(
                            std::time::Duration::from_secs(interval_secs)
                        );
                        tracing::info!("Rate limit monitor started (file: {file_path}, interval: {interval_secs}s)");
                        loop {
                            interval.tick().await;
                            // watch에서 최신 값이 있으면 갱신 (non-blocking)
                            ctx_rx.mark_changed();
                            let fresh_ctx = ctx_rx.borrow_and_update().clone();
                            match crate::ratelimit::read_ratelimit_file(&file_path) {
                                Some(info) => {
                                    let text = crate::ratelimit::RateLimitMonitor::format_presence(&info);
                                    fresh_ctx.set_activity(Some(
                                        poise::serenity_prelude::ActivityData::watching(&text)
                                    ));
                                    if let Some(channel_id) = notification_channel {
                                        monitor.check_and_alert(&info, &fresh_ctx, channel_id, lang).await;
                                    }
                                }
                                None => {
                                    fresh_ctx.set_activity(None);
                                }
                            }
                        }
                    });
                }

                // Idle session TTL sweep
                {
                    let sessions = Arc::clone(&sessions);
                    let idle_timeout = std::time::Duration::from_secs(config.claude.idle_timeout_secs);
                    let permission_rxs = Arc::clone(&permission_rxs);
                    let session_skills = Arc::clone(&session_skills);
                    let db_clone = db.clone();
                    let lang = config.language;
                    let mut ctx_rx = ctx_tx.subscribe();
                    tokio::spawn(async move {
                        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
                        loop {
                            interval.tick().await;
                            let evicted = sessions.sweep_idle_sessions(idle_timeout).await;
                            if evicted.is_empty() {
                                continue;
                            }
                            tracing::info!("TTL sweep: evicted {} sessions", evicted.len());
                            for tid in &evicted {
                                permission_rxs.lock().await.remove(tid);
                                session_skills.lock().await.remove(tid);
                                db::repository::update_session_status(&db_clone, tid, "idle").await.ok();
                                if let Ok(channel_id) = tid.parse::<u64>() {
                                    ctx_rx.mark_changed();
                                    let ctx = ctx_rx.borrow_and_update().clone();
                                    poise::serenity_prelude::ChannelId::new(channel_id)
                                        .say(&ctx, format!("-# ⏰ {}", lang.session_idle_cleaned()))
                                        .await
                                        .ok();
                                }
                            }
                        }
                    });
                }

                Ok(Data {
                    config,
                    db,
                    sessions,
                    permission_rxs,
                    pending_permissions,
                    session_skills,
                    skill_descriptions,
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
