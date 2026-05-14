use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::FromRow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, FromRow)]
struct Project {
    channel_id: String,
    path: String,
    name: Option<String>,
    disallowed_tools: Option<String>,
    created_at: String,
}

#[derive(Debug, FromRow)]
struct Session {
    thread_id: String,
    channel_id: String,
    session_id: Option<String>,
    status: String,
    created_at: String,
    last_active_at: Option<String>,
    model: Option<String>,
}

/// Where a resolved SQLite path came from.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolutionSource {
    EnvVar,
    ConfigToml,
    Default,
}

impl ResolutionSource {
    fn label(&self) -> &'static str {
        match self {
            ResolutionSource::EnvVar => "PIDORY_LEGACY_DB env var",
            ResolutionSource::ConfigToml => "config.toml [database] path",
            ResolutionSource::Default => "built-in default",
        }
    }
}

/// Result of `resolve_sqlite_path`.
#[derive(Debug)]
pub enum SqlitePathResolution {
    Found(PathBuf, ResolutionSource),
    NotFound {
        tried: Vec<(PathBuf, ResolutionSource)>,
    },
}

/// Resolve the legacy SQLite database path via fallback chain:
///   1. `env`  — `PIDORY_LEGACY_DB` env var value (if Some)
///   2. `config_db_path` — `[database] path` from config.toml (if Some and non-empty)
///      - relative paths are resolved against `config_dir`
///   3. `default` — built-in default path
///
/// Returns `Found` if the first match exists on disk, `NotFound` (with all
/// candidates tried) otherwise.
pub fn resolve_sqlite_path(
    env: Option<&str>,
    config_db_path: Option<&Path>,
    config_dir: &Path,
    default: &Path,
) -> SqlitePathResolution {
    let mut tried: Vec<(PathBuf, ResolutionSource)> = Vec::new();

    // 1. PIDORY_LEGACY_DB env var
    if let Some(raw) = env {
        let p = PathBuf::from(raw);
        if p.exists() {
            return SqlitePathResolution::Found(p.clone(), ResolutionSource::EnvVar);
        }
        tried.push((p, ResolutionSource::EnvVar));
    }

    // 2. config.toml [database] path (non-empty)
    if let Some(cfg_raw) = config_db_path.filter(|p| !p.as_os_str().is_empty()) {
        let p = if cfg_raw.is_absolute() {
            cfg_raw.to_path_buf()
        } else {
            config_dir.join(cfg_raw)
        };
        if p.exists() {
            return SqlitePathResolution::Found(p.clone(), ResolutionSource::ConfigToml);
        }
        tried.push((p, ResolutionSource::ConfigToml));
    }

    // 3. Built-in default
    {
        let p = default.to_path_buf();
        if p.exists() {
            return SqlitePathResolution::Found(p.clone(), ResolutionSource::Default);
        }
        tried.push((p, ResolutionSource::Default));
    }

    SqlitePathResolution::NotFound { tried }
}

#[cfg(test)]
mod path_resolution_tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: create a real file inside a TempDir and return its path.
    fn touch(dir: &TempDir, name: &str) -> PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, b"").expect("touch failed");
        p
    }

    // T1: env var is set and the file exists → Found(_, EnvVar)
    #[test]
    fn env_var_path_takes_precedence() {
        let dir = TempDir::new().unwrap();
        let env_file = touch(&dir, "env.db");
        let config_file = touch(&dir, "config.db");
        let default_file = touch(&dir, "default.db");

        let result = resolve_sqlite_path(
            Some(env_file.to_str().unwrap()),
            Some(config_file.as_path()),
            dir.path(),
            &default_file,
        );

        assert!(
            matches!(result, SqlitePathResolution::Found(_, ResolutionSource::EnvVar)),
            "expected Found(_, EnvVar), got {result:?}"
        );
    }

    // T2: env absent, config path set and file exists → Found(_, ConfigToml)
    #[test]
    fn config_path_used_when_env_absent() {
        let dir = TempDir::new().unwrap();
        let config_file = touch(&dir, "config.db");
        let default_file = touch(&dir, "default.db");

        let result = resolve_sqlite_path(
            None,
            Some(config_file.as_path()),
            dir.path(),
            &default_file,
        );

        assert!(
            matches!(result, SqlitePathResolution::Found(_, ResolutionSource::ConfigToml)),
            "expected Found(_, ConfigToml), got {result:?}"
        );
    }

    // T3: env None + config None + default exists → Found(_, Default)
    #[test]
    fn default_path_used_when_both_absent() {
        let dir = TempDir::new().unwrap();
        let default_file = touch(&dir, "default.db");

        let result = resolve_sqlite_path(None, None, dir.path(), &default_file);

        assert!(
            matches!(result, SqlitePathResolution::Found(_, ResolutionSource::Default)),
            "expected Found(_, Default), got {result:?}"
        );
    }

    // T4: all candidates absent → NotFound with tried list containing all attempted paths
    #[test]
    fn not_found_returns_all_tried() {
        let dir = TempDir::new().unwrap();
        // These paths point inside the tempdir but the files do NOT exist.
        let env_path = dir.path().join("missing_env.db");
        let config_path = dir.path().join("missing_config.db");
        let default_path = dir.path().join("missing_default.db");

        let result = resolve_sqlite_path(
            Some(env_path.to_str().unwrap()),
            Some(config_path.as_path()),
            dir.path(),
            &default_path,
        );

        match result {
            SqlitePathResolution::NotFound { tried } => {
                let sources: Vec<&ResolutionSource> =
                    tried.iter().map(|(_, src)| src).collect();
                assert!(
                    sources.contains(&&ResolutionSource::EnvVar),
                    "tried list missing EnvVar: {sources:?}"
                );
                assert!(
                    sources.contains(&&ResolutionSource::ConfigToml),
                    "tried list missing ConfigToml: {sources:?}"
                );
                assert!(
                    sources.contains(&&ResolutionSource::Default),
                    "tried list missing Default: {sources:?}"
                );
                assert_eq!(tried.len(), 3, "expected 3 entries in tried, got {}", tried.len());
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // T5: relative config path resolves against config_dir
    #[test]
    fn relative_config_path_resolves_against_config_dir() {
        let dir = TempDir::new().unwrap();
        // Create the DB file at <dir>/sub/rel.db
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let db_file = sub.join("rel.db");
        std::fs::write(&db_file, b"").unwrap();

        // Pass relative path "sub/rel.db" — should be joined to config_dir
        let rel: &Path = Path::new("sub/rel.db");
        let default_path = dir.path().join("missing_default.db");

        let result = resolve_sqlite_path(None, Some(rel), dir.path(), &default_path);

        match result {
            SqlitePathResolution::Found(resolved, ResolutionSource::ConfigToml) => {
                assert_eq!(resolved, db_file, "resolved path mismatch");
            }
            other => panic!("expected Found(_, ConfigToml), got {other:?}"),
        }
    }
}

/// Minimal config struct for reading `[database] path` from config.toml.
/// Avoids importing the main pidory config (which depends on Discord / i18n modules).
#[derive(serde::Deserialize, Default)]
struct MigrateConfig {
    #[serde(default)]
    database: MigrateDatabaseConfig,
}

#[derive(serde::Deserialize, Default)]
struct MigrateDatabaseConfig {
    #[serde(default)]
    path: String,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("migration failed: {e:?}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    // Step 2: DATABASE_URL
    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL environment variable not set")?;

    // Step 3: postgres connect
    let pg_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .context("failed to connect to postgres")?;

    // Step 4: run migrations so tables exist in a fresh environment
    sqlx::migrate!("./migrations")
        .run(&pg_pool)
        .await
        .context("failed to run migrations")?;

    // Step 5: check if postgres already has data (idempotent guard)
    let projects_count: i64 = sqlx::query_scalar("SELECT count(*) FROM projects")
        .fetch_one(&pg_pool)
        .await
        .context("failed to count projects")?;

    let sessions_count: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&pg_pool)
        .await
        .context("failed to count sessions")?;

    if projects_count > 0 || sessions_count > 0 {
        tracing::info!(
            "postgres already populated ({projects_count} projects, {sessions_count} sessions), skipping migration"
        );
        return Ok(());
    }

    // Step 6: resolve legacy sqlite path via fallback chain
    let config_path_str =
        std::env::var("PIDORY_CONFIG").unwrap_or_else(|_| "./config.toml".to_string());
    let config_path = PathBuf::from(&config_path_str);
    let config_dir = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // Read config.toml [database] path if the file exists (best-effort; warn on parse errors)
    let config_db_path: Option<PathBuf> = if config_path.exists() {
        match std::fs::read_to_string(&config_path) {
            Ok(contents) => match toml::from_str::<MigrateConfig>(&contents) {
                Ok(cfg) if !cfg.database.path.is_empty() => {
                    Some(PathBuf::from(&cfg.database.path))
                }
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!(
                        "failed to parse {} for [database] path: {e}",
                        config_path.display()
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    "failed to read {}: {e}",
                    config_path.display()
                );
                None
            }
        }
    } else {
        None
    };

    let env_val = std::env::var("PIDORY_LEGACY_DB").ok();
    let default_path = PathBuf::from("/var/lib/pidory/pidory.db");

    let resolution = resolve_sqlite_path(
        env_val.as_deref(),
        config_db_path.as_deref(),
        &config_dir,
        &default_path,
    );

    let sqlite_path = match resolution {
        SqlitePathResolution::Found(p, source) => {
            tracing::info!(
                "using sqlite db at {} (from {})",
                p.display(),
                source.label()
            );
            p
        }
        SqlitePathResolution::NotFound { tried } => {
            let lines = tried
                .iter()
                .map(|(p, src)| format!("  - {} ({})", p.display(), src.label()))
                .collect::<Vec<_>>()
                .join("\n");
            tracing::error!(
                "legacy sqlite db not found. Tried:\n{lines}\n\
                To fix, set one of:\n  \
                  PIDORY_LEGACY_DB=/path/to/pidory.db  (env var)\n  \
                  [database]\n  path = \"/path/to/pidory.db\"  (in config.toml)"
            );
            // exit code 2 = configuration error; avoids systemd Restart=on-failure loop
            std::process::exit(2);
        }
    };

    // Step 8: sqlite read-only connect
    let opts = SqliteConnectOptions::new()
        .filename(&sqlite_path)
        .read_only(true);
    let sqlite_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .with_context(|| format!("failed to open sqlite db at {}", sqlite_path.display()))?;

    // Step 8b: introspect sessions table for model column
    let has_model_col = {
        let rows = sqlx::query("PRAGMA table_info(sessions)")
            .fetch_all(&sqlite_pool)
            .await
            .context("failed to run PRAGMA table_info(sessions)")?;
        rows.iter().any(|row| {
            use sqlx::Row;
            row.try_get::<String, _>("name")
                .map(|n| n == "model")
                .unwrap_or(false)
        })
    };

    // Step 9: read all rows from sqlite
    let projects: Vec<Project> = sqlx::query_as::<_, Project>(
        "SELECT channel_id, path, name, disallowed_tools, created_at FROM projects",
    )
    .fetch_all(&sqlite_pool)
    .await
    .context("failed to read projects from sqlite")?;

    let sessions: Vec<Session> = if has_model_col {
        sqlx::query_as::<_, Session>(
            "SELECT thread_id, channel_id, session_id, status, created_at, last_active_at, model FROM sessions",
        )
        .fetch_all(&sqlite_pool)
        .await
        .context("failed to read sessions from sqlite")?
    } else {
        sqlx::query_as::<_, Session>(
            "SELECT thread_id, channel_id, session_id, status, created_at, last_active_at, NULL AS model FROM sessions",
        )
        .fetch_all(&sqlite_pool)
        .await
        .context("failed to read sessions from sqlite (no model column)")?
    };

    let projects_n = projects.len();
    let sessions_n = sessions.len();

    tracing::info!("read {projects_n} projects and {sessions_n} sessions from sqlite");

    // Build set of imported channel_ids for FK validation (c1)
    let channel_ids: HashSet<String> = projects.iter().map(|p| p.channel_id.clone()).collect();

    // Step 10: BEGIN transaction and INSERT all rows
    let mut tx = pg_pool
        .begin()
        .await
        .context("failed to begin transaction")?;

    for p in &projects {
        sqlx::query(
            "INSERT INTO projects (channel_id, path, name, disallowed_tools, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&p.channel_id)
        .bind(&p.path)
        .bind(&p.name)
        .bind(&p.disallowed_tools)
        .bind(&p.created_at)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("failed to insert project {}", p.channel_id))?;
    }

    let mut skipped_sessions = 0usize;
    for s in &sessions {
        if !channel_ids.contains(&s.channel_id) {
            skipped_sessions += 1;
            continue;
        }
        sqlx::query(
            "INSERT INTO sessions (thread_id, channel_id, session_id, status, created_at, last_active_at, model) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&s.thread_id)
        .bind(&s.channel_id)
        .bind(&s.session_id)
        .bind(&s.status)
        .bind(&s.created_at)
        .bind(&s.last_active_at)
        .bind(&s.model)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("failed to insert session {}", s.thread_id))?;
    }

    if skipped_sessions > 0 {
        tracing::warn!(
            "skipped {skipped_sessions} orphan sessions (channel_id not in projects)"
        );
    }

    // Step 11: COMMIT
    tx.commit().await.context("failed to commit transaction")?;

    // Drop sqlite pool before renaming the file
    drop(sqlite_pool);

    // Rename sqlite file to backup — only on success
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut backup_path = sqlite_path.clone().into_os_string();
    backup_path.push(format!(".migrated.{ts}.bak"));
    let backup_path = PathBuf::from(backup_path);
    std::fs::rename(&sqlite_path, &backup_path)
        .with_context(|| "migration succeeded but failed to rename sqlite file")?;

    tracing::info!(
        "migration complete: {projects_n} projects, {sessions_n} sessions — sqlite backed up to {}",
        backup_path.display()
    );

    Ok(())
}
