use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::FromRow;
use std::collections::HashSet;
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

    // Step 6: legacy sqlite path
    let sqlite_path = std::env::var("PIDORY_LEGACY_DB")
        .unwrap_or_else(|_| "/var/lib/pidory/pidory.db".to_string());

    // Step 7: sqlite file existence check
    if !std::path::Path::new(&sqlite_path).exists() {
        tracing::info!("no legacy sqlite db found at {sqlite_path}, nothing to migrate");
        return Ok(());
    }

    // Step 8: sqlite read-only connect
    let sqlite_url = format!("sqlite://{sqlite_path}?mode=ro");
    let sqlite_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&sqlite_url)
        .await
        .with_context(|| format!("failed to open sqlite db at {sqlite_path}"))?;

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
    let backup_path = format!("{sqlite_path}.migrated.{ts}.bak");
    std::fs::rename(&sqlite_path, &backup_path).with_context(|| {
        format!("migration succeeded but failed to rename sqlite file")
    })?;

    tracing::info!(
        "migration complete: {projects_n} projects, {sessions_n} sessions — sqlite backed up to {backup_path}"
    );

    Ok(())
}
