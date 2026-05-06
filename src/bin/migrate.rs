use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::FromRow;
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

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Step 2: DATABASE_URL
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("DATABASE_URL environment variable not set");
            std::process::exit(1);
        }
    };

    // Step 3: postgres connect
    let pg_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .map_err(|e| format!("failed to connect to postgres: {e}"))?;

    // Step 4: run migrations so tables exist in a fresh environment
    sqlx::migrate!("./migrations")
        .run(&pg_pool)
        .await
        .map_err(|e| format!("failed to run migrations: {e}"))?;

    // Step 5: check if postgres already has data (idempotent guard)
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM projects")
        .fetch_one(&pg_pool)
        .await
        .map_err(|e| format!("failed to count projects: {e}"))?;

    if count > 0 {
        tracing::info!("postgres already populated ({count} projects), skipping migration");
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
        .map_err(|e| format!("failed to open sqlite db at {sqlite_path}: {e}"))?;

    // Step 9: read all rows from sqlite
    let projects: Vec<Project> = sqlx::query_as::<_, Project>(
        "SELECT channel_id, path, name, disallowed_tools, created_at FROM projects",
    )
    .fetch_all(&sqlite_pool)
    .await
    .map_err(|e| format!("failed to read projects from sqlite: {e}"))?;

    let sessions: Vec<Session> = sqlx::query_as::<_, Session>(
        "SELECT thread_id, channel_id, session_id, status, created_at, last_active_at, model FROM sessions",
    )
    .fetch_all(&sqlite_pool)
    .await
    .map_err(|e| format!("failed to read sessions from sqlite: {e}"))?;

    let projects_n = projects.len();
    let sessions_n = sessions.len();

    tracing::info!("read {projects_n} projects and {sessions_n} sessions from sqlite");

    // Step 10: BEGIN transaction and INSERT all rows
    let mut tx = pg_pool
        .begin()
        .await
        .map_err(|e| format!("failed to begin transaction: {e}"))?;

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
        .map_err(|e| format!("failed to insert project {}: {e}", p.channel_id))?;
    }

    for s in &sessions {
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
        .map_err(|e| format!("failed to insert session {}: {e}", s.thread_id))?;
    }

    // Step 11: COMMIT
    tx.commit()
        .await
        .map_err(|e| format!("failed to commit transaction: {e}"))?;

    // Drop sqlite pool before renaming the file
    drop(sqlite_pool);

    // Rename sqlite file to backup — only on success
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup_path = format!("{sqlite_path}.migrated.{ts}.bak");
    std::fs::rename(&sqlite_path, &backup_path)
        .map_err(|e| format!("migration succeeded but failed to rename sqlite file: {e}"))?;

    tracing::info!(
        "migration complete: {projects_n} projects, {sessions_n} sessions — sqlite backed up to {backup_path}"
    );

    Ok(())
}
