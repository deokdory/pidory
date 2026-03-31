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
}

#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    pub guild_id: u64,
    pub owner_id: u64,
    // token은 환경변수 PIDORY_DISCORD_TOKEN으로 설정
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

fn default_subprocess_timeout_secs() -> u64 {
    600
}

fn default_max_concurrent() -> usize {
    6
}

fn default_max_sessions() -> usize {
    10
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
}
