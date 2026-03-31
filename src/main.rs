mod commands;
mod config;
mod db;
mod error;
mod handler;
mod subprocess;

use std::collections::{HashMap, HashSet};
use commands::skill::load_skill_descriptions;
use std::sync::Arc;

use poise::serenity_prelude as serenity;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, mpsc, oneshot};
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

    let token = std::env::var("PIDORY_DISCORD_TOKEN")
        .map_err(|_| PidoryError::Config("PIDORY_DISCORD_TOKEN environment variable not set".to_string()))?;

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

                let db = db::init_pool("pidory.db").await?;

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

                Ok(Data {
                    config,
                    db,
                    sessions,
                    permission_rxs,
                    pending_permissions,
                    session_skills,
                    skill_descriptions,
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
