use serde::Deserialize;
use std::fs;

use crate::error::PidoryError;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub discord: DiscordConfig,
    pub claude: ClaudeConfig,
    pub response: ResponseConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub ratelimit: RateLimitConfig,
}

#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    pub guild_id: u64,
    pub owner_id: u64,
    #[serde(default = "default_token_env")]
    pub token_env: String,
    #[serde(default)]
    pub notification_channel_id: Option<u64>,
}

fn default_token_env() -> String {
    "PIDORY_DISCORD_TOKEN".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeConfig {
    pub binary_path: String,
    #[serde(default)]
    pub default_disallowed_tools: Vec<String>,
    #[serde(default = "default_subprocess_timeout_secs")]
    pub subprocess_timeout_secs: u64,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
pub struct ResponseConfig {
    #[serde(default = "default_max_chunk_length")]
    pub max_chunk_length: usize,
    #[serde(default = "default_max_chunks")]
    pub max_chunks: usize,
}

#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_db_path")]
    pub path: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_db_path(),
        }
    }
}

fn default_db_path() -> String {
    "pidory.db".to_string()
}

#[derive(Debug, Deserialize)]
pub struct RateLimitConfig {
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default = "default_update_interval_secs")]
    pub update_interval_secs: u64,
    #[serde(default = "default_alert_thresholds")]
    pub alert_thresholds: Vec<u8>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            file_path: None,
            update_interval_secs: default_update_interval_secs(),
            alert_thresholds: default_alert_thresholds(),
        }
    }
}

fn default_update_interval_secs() -> u64 {
    60
}

fn default_alert_thresholds() -> Vec<u8> {
    vec![50, 80]
}

fn default_subprocess_timeout_secs() -> u64 {
    600
}

fn default_max_concurrent() -> usize {
    6
}

fn default_max_sessions() -> usize {
    10
}

fn default_idle_timeout_secs() -> u64 {
    7200
}

fn default_max_chunk_length() -> usize {
    1900
}

fn default_max_chunks() -> usize {
    10
}

impl Config {
    pub fn load(path: &str) -> Result<Config, PidoryError> {
        let content = fs::read_to_string(path)
            .map_err(|e| PidoryError::Config(format!("Failed to read config file '{}': {}", path, e)))?;

        let config: Config = toml::from_str(&content)
            .map_err(|e| PidoryError::Config(format!("Failed to parse config file '{}': {}", path, e)))?;

        if config.discord.token_env.trim().is_empty() {
            return Err(PidoryError::Config("discord.token_env must not be empty".to_string()));
        }
        if config.database.path.trim().is_empty() {
            return Err(PidoryError::Config("database.path must not be empty".to_string()));
        }

        Ok(config)
    }

    /// 환경변수 PIDORY_CONFIG로 경로를 지정하거나, 없으면 ./config.toml 사용
    pub fn load_from_env() -> Result<Config, PidoryError> {
        let path = std::env::var("PIDORY_CONFIG").unwrap_or_else(|_| "./config.toml".to_string());
        Self::load(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_valid_config() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.discord.guild_id, 123);
        assert_eq!(config.discord.owner_id, 456);
        assert_eq!(config.claude.binary_path, "claude");
        // defaults
        assert_eq!(config.claude.subprocess_timeout_secs, 600);
        assert_eq!(config.claude.max_concurrent, 6);
        assert_eq!(config.response.max_chunk_length, 1900);
        assert_eq!(config.response.max_chunks, 10);
        assert_eq!(config.database.path, "pidory.db");
        assert_eq!(config.discord.token_env, "PIDORY_DISCORD_TOKEN");
    }

    #[test]
    fn parse_config_with_all_fields() {
        let toml_str = r#"
[discord]
guild_id = 111
owner_id = 222

[claude]
binary_path = "/usr/bin/claude"
default_disallowed_tools = ["Bash", "Edit"]
subprocess_timeout_secs = 300
max_concurrent = 4

[response]
max_chunk_length = 1800
max_chunks = 5
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.claude.default_disallowed_tools, vec!["Bash", "Edit"]);
        assert_eq!(config.claude.subprocess_timeout_secs, 300);
        assert_eq!(config.response.max_chunk_length, 1800);
        assert_eq!(config.database.path, "pidory.db"); // default when [database] omitted
        assert_eq!(config.discord.token_env, "PIDORY_DISCORD_TOKEN"); // default
    }

    #[test]
    fn parse_config_with_token_env() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456
token_env = "PIDORY_DEV_DISCORD_TOKEN"

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.discord.token_env, "PIDORY_DEV_DISCORD_TOKEN");
    }

    #[test]
    fn parse_config_with_database_path() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[database]
path = "pidory-dev.db"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.database.path, "pidory-dev.db");
    }

    #[test]
    fn load_from_file() {
        let dir = std::env::temp_dir().join("pidory_test_config");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("test_config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, r#"
[discord]
guild_id = 1
owner_id = 2
[claude]
binary_path = "claude"
[response]
"#).unwrap();

        let config = Config::load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.discord.guild_id, 1);

        // cleanup
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_missing_file() {
        let result = Config::load("/nonexistent/path/config.toml");
        assert!(result.is_err());
    }

    #[test]
    fn reject_empty_token_env() {
        let dir = std::env::temp_dir().join("pidory_test_empty_token");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("bad_config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, r#"
[discord]
guild_id = 1
owner_id = 2
token_env = ""
[claude]
binary_path = "claude"
[response]
"#).unwrap();
        let result = Config::load(path.to_str().unwrap());
        assert!(result.is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn parse_config_without_ratelimit() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.ratelimit.file_path.is_none());
        assert_eq!(config.ratelimit.update_interval_secs, 60);
        assert_eq!(config.ratelimit.alert_thresholds, vec![50, 80]);
    }

    #[test]
    fn parse_config_with_ratelimit() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]

[ratelimit]
file_path = "/tmp/pidory-ratelimits.json"
update_interval_secs = 30
alert_thresholds = [70, 90]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.ratelimit.file_path.as_deref(),
            Some("/tmp/pidory-ratelimits.json")
        );
        assert_eq!(config.ratelimit.update_interval_secs, 30);
        assert_eq!(config.ratelimit.alert_thresholds, vec![70, 90]);
    }

    #[test]
    fn reject_empty_db_path() {
        let dir = std::env::temp_dir().join("pidory_test_empty_db");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("bad_config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, r#"
[discord]
guild_id = 1
owner_id = 2
[claude]
binary_path = "claude"
[database]
path = ""
[response]
"#).unwrap();
        let result = Config::load(path.to_str().unwrap());
        assert!(result.is_err());
        std::fs::remove_file(&path).ok();
    }
}
