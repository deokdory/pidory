use serde::Deserialize;
use std::fs;

use crate::error::PidoryError;
use crate::i18n::Lang;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FooterConfig {
    #[serde(default)]
    pub show_context_percent: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimestampConfig {
    #[serde(default = "default_timestamp_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub tz: Option<String>,
}

impl Default for TimestampConfig {
    fn default() -> Self {
        Self {
            enabled: default_timestamp_enabled(),
            tz: None,
        }
    }
}

fn default_timestamp_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub language: Lang,
    pub discord: DiscordConfig,
    pub claude: ClaudeConfig,
    pub response: ResponseConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub release: ReleaseConfig,
    #[serde(default)]
    pub attachment: AttachmentConfig,
    #[serde(default)]
    pub backup: BackupConfig,
    #[serde(default)]
    pub footer: FooterConfig,
    #[serde(default)]
    pub timestamp: TimestampConfig,
    #[serde(default)]
    pub permission: PermissionConfig,
}

#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    pub guild_id: u64,
    pub owner_id: u64,
    #[serde(default = "default_token_env")]
    pub token_env: String,
    #[serde(default)]
    pub notification_channel_id: Option<u64>,
    #[serde(default)]
    pub project_roots: Vec<String>,
    #[serde(default)]
    pub default_category_id: Option<String>,
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
    #[serde(default)]
    pub default_model: Option<String>,
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
pub struct BackupConfig {
    #[serde(default = "default_backup_dir")]
    pub dir: String,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            dir: default_backup_dir(),
        }
    }
}

#[cfg(target_os = "linux")]
fn default_backup_dir() -> String {
    "/var/lib/pidory/backups".to_string()
}

#[cfg(target_os = "macos")]
fn default_backup_dir() -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => format!("{}/.pidory/backups", home),
        _ => "./backups".to_string(),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("pidory only supports Linux and macOS targets (default_backup_dir)");

#[derive(Debug, Deserialize)]
pub struct ReleaseConfig {
    #[serde(default = "default_release_enabled")]
    pub enabled: bool,
    #[serde(default = "default_release_repo")]
    pub repo: String,
    #[serde(default = "default_release_check_interval_secs")]
    pub check_interval_secs: u64,
    #[serde(default = "default_release_last_tag_file")]
    pub last_tag_file: String,
    #[serde(default)]
    pub token_env: Option<String>,
}

impl Default for ReleaseConfig {
    fn default() -> Self {
        Self {
            enabled: default_release_enabled(),
            repo: default_release_repo(),
            check_interval_secs: default_release_check_interval_secs(),
            last_tag_file: default_release_last_tag_file(),
            token_env: None,
        }
    }
}

fn default_release_enabled() -> bool {
    true
}

fn default_release_repo() -> String {
    "deokdory/pidory".to_string()
}

fn default_release_check_interval_secs() -> u64 {
    21600
}

fn default_release_last_tag_file() -> String {
    "/tmp/pidory-last-release.txt".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct PermissionConfig {
    #[serde(default = "default_permission_response_timeout")]
    pub response_timeout_secs: u64,
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            response_timeout_secs: default_permission_response_timeout(),
        }
    }
}

fn default_permission_response_timeout() -> u64 {
    300
}

#[derive(Debug, Deserialize)]
pub struct AttachmentConfig {
    #[serde(default = "default_max_file_size_mb")]
    pub max_file_size_mb: u64,
    #[serde(default = "default_max_aggregate_size_mb")]
    pub max_aggregate_size_mb: u64,
    #[serde(default = "default_download_timeout_secs_attachment")]
    pub download_timeout_secs: u64,
}

impl Default for AttachmentConfig {
    fn default() -> Self {
        Self {
            max_file_size_mb: default_max_file_size_mb(),
            max_aggregate_size_mb: default_max_aggregate_size_mb(),
            download_timeout_secs: default_download_timeout_secs_attachment(),
        }
    }
}

impl AttachmentConfig {
    pub fn max_file_size_bytes(&self) -> u64 {
        self.max_file_size_mb.saturating_mul(1024 * 1024)
    }

    pub fn max_aggregate_size_bytes(&self) -> u64 {
        self.max_aggregate_size_mb.saturating_mul(1024 * 1024)
    }
}

fn default_max_file_size_mb() -> u64 {
    25
}

fn default_max_aggregate_size_mb() -> u64 {
    50
}

fn default_download_timeout_secs_attachment() -> u64 {
    30
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

pub(crate) fn normalize_project_roots(roots: &[String]) -> Result<Vec<String>, PidoryError> {
    roots
        .iter()
        .map(|root| {
            // Expand leading ~ to $HOME
            let expanded = if root == "~" {
                std::env::var("HOME").map_err(|_| {
                    PidoryError::Config(format!(
                        "project_roots contains '{}' but HOME is not set", root
                    ))
                })?
            } else if root.starts_with("~/") {
                let home = std::env::var("HOME").map_err(|_| {
                    PidoryError::Config(format!(
                        "project_roots contains '{}' but HOME is not set", root
                    ))
                })?;
                format!("{}{}", home, &root[1..])
            } else {
                root.clone()
            };

            // Try to canonicalize; fall back to expanded path on failure
            let resolved = match std::fs::canonicalize(&expanded) {
                Ok(canonical) => canonical.to_string_lossy().into_owned(),
                Err(_) => {
                    tracing::warn!("project root not found, using as-is: {}", expanded);
                    expanded
                }
            };

            // Remove trailing slash, but preserve bare "/"
            if resolved.len() > 1 {
                Ok(resolved.trim_end_matches('/').to_string())
            } else {
                Ok(resolved)
            }
        })
        .collect()
}

impl Config {
    /// backup directory 결정 — `[backup]` 명시 우선, 미명시 + legacy `[database]` 가 비 default 면
    /// 경고 후 legacy parent 사용, 그 외 OS-specific default.
    pub fn resolve_backup_dir(&self) -> std::path::PathBuf {
        use std::path::{Path, PathBuf};
        let backup = self.backup.dir.as_str();
        let backup_default = default_backup_dir();
        let database_default = default_db_path();
        if backup == backup_default
            && self.database.path != database_default
            && let Some(parent) = Path::new(&self.database.path).parent()
            && !parent.as_os_str().is_empty()
        {
            tracing::warn!(
                "legacy database.path detected ({:?}) — backup_dir falling back to legacy parent {:?}. Set [backup] dir in config.toml to silence this warning.",
                self.database.path,
                parent
            );
            return parent.to_path_buf();
        }
        PathBuf::from(backup)
    }

    pub fn load(path: &str) -> Result<Config, PidoryError> {
        let content = fs::read_to_string(path)
            .map_err(|e| PidoryError::Config(format!("Failed to read config file '{}': {}", path, e)))?;

        let mut config: Config = toml::from_str(&content)
            .map_err(|e| PidoryError::Config(format!("Failed to parse config file '{}': {}", path, e)))?;

        if config.discord.token_env.trim().is_empty() {
            return Err(PidoryError::Config("discord.token_env must not be empty".to_string()));
        }
        if config.backup.dir.trim().is_empty() {
            return Err(PidoryError::Config("backup.dir must not be empty".to_string()));
        }
        // database.path is deprecated; DATABASE_URL env is the authoritative source.
        // Validation removed to avoid spurious errors when [database] section is omitted.

        config.discord.project_roots = normalize_project_roots(&config.discord.project_roots)?;

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
        assert_eq!(config.language, Lang::Ko); // default
        assert_eq!(config.attachment.max_file_size_mb, 25);
        assert_eq!(config.attachment.max_aggregate_size_mb, 50);
        assert_eq!(config.attachment.download_timeout_secs, 30);
        assert!(config.discord.project_roots.is_empty());
        assert!(config.discord.default_category_id.is_none());
        assert!(!config.footer.show_context_percent);
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
    fn empty_db_path_now_allowed() {
        // database.path is deprecated; DATABASE_URL env is the authoritative source.
        // An empty path no longer causes a config error.
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
        assert!(result.is_ok());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn parse_config_with_language_en() {
        let toml_str = r#"
language = "en"

[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.language, Lang::En);
    }

    #[test]
    fn parse_config_without_language_defaults_ko() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.language, Lang::Ko);
    }

    #[test]
    fn parse_config_with_attachment() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]

[attachment]
max_file_size_mb = 50
max_aggregate_size_mb = 100
download_timeout_secs = 60
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.attachment.max_file_size_mb, 50);
        assert_eq!(config.attachment.max_aggregate_size_mb, 100);
        assert_eq!(config.attachment.download_timeout_secs, 60);
        assert_eq!(config.attachment.max_file_size_bytes(), 50 * 1024 * 1024);
        assert_eq!(config.attachment.max_aggregate_size_bytes(), 100 * 1024 * 1024);
    }

    #[test]
    fn parse_config_without_attachment_defaults() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.attachment.max_file_size_mb, 25);
        assert_eq!(config.attachment.max_aggregate_size_mb, 50);
        assert_eq!(config.attachment.download_timeout_secs, 30);
        assert_eq!(config.attachment.max_file_size_bytes(), 25 * 1024 * 1024);
        assert_eq!(config.attachment.max_aggregate_size_bytes(), 50 * 1024 * 1024);
    }

    #[test]
    fn parse_config_without_project_roots_defaults_empty() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.discord.project_roots.is_empty());
    }

    #[test]
    fn parse_config_with_project_roots() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456
project_roots = ["/home/user/projects", "/opt/work"]

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.discord.project_roots, vec!["/home/user/projects", "/opt/work"]);
    }

    #[test]
    fn parse_config_without_category_defaults_none() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.discord.default_category_id.is_none());
    }

    #[test]
    fn parse_config_with_category() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456
default_category_id = "123456789"

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.discord.default_category_id, Some("123456789".to_string()));
    }

    #[test]
    fn normalize_tilde_expansion() {
        let home = std::env::var("HOME").expect("HOME must be set");
        let result = normalize_project_roots(&["~/test-nonexistent-12345".to_string()]).unwrap();
        assert_eq!(result, vec![format!("{}/test-nonexistent-12345", home)]);
    }

    #[test]
    fn normalize_trailing_slash_removed() {
        let result = normalize_project_roots(&["/tmp/".to_string()]).unwrap();
        assert!(!result[0].ends_with('/'));
        assert!(result[0].starts_with('/'));
    }

    #[test]
    fn normalize_absolute_path_unchanged() {
        let canonical = std::fs::canonicalize("/tmp").unwrap();
        let expected = canonical.to_string_lossy().into_owned();
        let result = normalize_project_roots(&["/tmp".to_string()]).unwrap();
        assert_eq!(result, vec![expected]);
    }

    #[test]
    fn normalize_nonexistent_path_kept() {
        let path = "/nonexistent-path-xyz-12345".to_string();
        let result = normalize_project_roots(std::slice::from_ref(&path)).unwrap();
        assert_eq!(result, vec![path]);
    }

    #[test]
    fn normalize_empty_list() {
        let result = normalize_project_roots(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn normalize_root_slash_preserved() {
        let result = normalize_project_roots(&["/".to_string()]).unwrap();
        assert_eq!(result, vec!["/"]);
    }

    #[test]
    fn parse_config_without_footer_defaults_off() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.footer.show_context_percent);
    }

    // ── W1-A: TimestampConfig 3 case ────────────────────────────────────────

    #[test]
    fn timestamp_config_default_enabled_true_tz_none() {
        // default() → enabled=true, tz=None
        let cfg = TimestampConfig::default();
        assert!(cfg.enabled, "default enabled must be true");
        assert!(cfg.tz.is_none(), "default tz must be None");
    }

    #[test]
    fn timestamp_config_enabled_false() {
        // [timestamp] enabled = false
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]

[timestamp]
enabled = false
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.timestamp.enabled, "enabled=false must parse correctly");
        assert!(config.timestamp.tz.is_none(), "tz must be None when omitted");
    }

    #[test]
    fn timestamp_config_tz_asia_seoul() {
        // [timestamp] tz = "Asia/Seoul"
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]

[timestamp]
tz = "Asia/Seoul"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.timestamp.enabled, "enabled must default to true even when tz is set");
        assert_eq!(config.timestamp.tz, Some("Asia/Seoul".to_string()));
    }

    #[test]
    fn parse_config_with_footer_show_context_percent_true() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]

[footer]
show_context_percent = true
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.footer.show_context_percent);
    }

    #[test]
    fn parse_config_with_backup_dir() {
        let toml_str = r#"
[discord]
guild_id = 1
owner_id = 2

[claude]
binary_path = "claude"

[response]

[backup]
dir = "/tmp/test/backups"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backup.dir, "/tmp/test/backups");
    }

    #[test]
    fn parse_config_without_backup_uses_default() {
        let toml_str = r#"
[discord]
guild_id = 1
owner_id = 2

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        #[cfg(target_os = "macos")]
        {
            let dir = &config.backup.dir;
            assert!(
                dir.ends_with("/.pidory/backups") || dir == "./backups",
                "macOS default 가 ~/.pidory/backups 또는 ./backups 여야 함. actual: {}",
                dir
            );
        }
        #[cfg(not(target_os = "macos"))]
        assert_eq!(config.backup.dir, "/var/lib/pidory/backups");
    }

    #[test]
    fn reject_empty_backup_dir() {
        let dir = std::env::temp_dir().join("pidory_test_empty_backup");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("bad_config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, r#"
[discord]
guild_id = 1
owner_id = 2
[claude]
binary_path = "claude"
[response]
[backup]
dir = ""
"#).unwrap();
        let result = Config::load(path.to_str().unwrap());
        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(
            err_msg.contains("backup.dir"),
            "에러 메시지에 'backup.dir' 포함되어야 함. actual: {}",
            err_msg
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn reject_whitespace_backup_dir() {
        let dir = std::env::temp_dir().join("pidory_test_ws_backup");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("bad_config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, r#"
[discord]
guild_id = 1
owner_id = 2
[claude]
binary_path = "claude"
[response]
[backup]
dir = "   "
"#).unwrap();
        let result = Config::load(path.to_str().unwrap());
        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("backup.dir"), "actual: {}", err_msg);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn legacy_database_path_fallback_for_backup_dir() {
        let toml_str = r#"
[discord]
guild_id = 1
owner_id = 2

[claude]
binary_path = "claude"

[response]

[database]
path = "/opt/legacy/pidory.db"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let resolved = config.resolve_backup_dir();
        assert_eq!(resolved.to_str().unwrap(), "/opt/legacy");
    }

    /// config_default_300:
    /// [permission] 섹션 없이 파싱 시 response_timeout_secs 기본값이 300.
    #[test]
    fn config_default_300() {
        let toml_str = r#"
[discord]
guild_id = 123
owner_id = 456

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.permission.response_timeout_secs, 300);
    }

    #[test]
    fn default_database_path_uses_backup_default() {
        let toml_str = r#"
[discord]
guild_id = 1
owner_id = 2

[claude]
binary_path = "claude"

[response]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let resolved = config.resolve_backup_dir();
        // database.path default ("pidory.db") + backup default → backup default 사용
        #[cfg(target_os = "linux")]
        assert_eq!(resolved.to_str().unwrap(), "/var/lib/pidory/backups");
        #[cfg(target_os = "macos")]
        assert!(
            resolved.to_str().unwrap().ends_with("/.pidory/backups")
                || resolved.to_str().unwrap() == "./backups"
        );
    }
}
