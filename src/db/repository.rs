use sqlx::PgPool;

use super::models::{Project, Session};
use crate::error::PidoryError;

// Project CRUD

pub async fn register_project(
    pool: &PgPool,
    channel_id: &str,
    path: &str,
    name: Option<&str>,
) -> Result<Project, PidoryError> {
    sqlx::query("INSERT INTO projects (channel_id, path, name) VALUES ($1, $2, $3)")
        .bind(channel_id)
        .bind(path)
        .bind(name)
        .execute(pool)
        .await
        .map_err(PidoryError::Db)?;

    get_project_by_channel(pool, channel_id)
        .await?
        .ok_or_else(|| PidoryError::NotFound(format!("project channel_id={channel_id}")))
}

pub async fn unregister_project(
    pool: &PgPool,
    channel_id: &str,
) -> Result<(), PidoryError> {
    sqlx::query("DELETE FROM projects WHERE channel_id = $1")
        .bind(channel_id)
        .execute(pool)
        .await
        .map_err(PidoryError::Db)?;

    Ok(())
}

pub async fn get_project_by_channel(
    pool: &PgPool,
    channel_id: &str,
) -> Result<Option<Project>, PidoryError> {
    sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE channel_id = $1")
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(PidoryError::Db)
}

#[allow(dead_code)]
pub async fn list_projects(pool: &PgPool) -> Result<Vec<Project>, PidoryError> {
    sqlx::query_as::<_, Project>("SELECT * FROM projects ORDER BY created_at DESC")
        .fetch_all(pool)
        .await
        .map_err(PidoryError::Db)
}

// Session CRUD

pub async fn create_session(
    pool: &PgPool,
    thread_id: &str,
    channel_id: &str,
) -> Result<Session, PidoryError> {
    sqlx::query("INSERT INTO sessions (thread_id, channel_id) VALUES ($1, $2)")
        .bind(thread_id)
        .bind(channel_id)
        .execute(pool)
        .await
        .map_err(PidoryError::Db)?;

    get_session_by_thread(pool, thread_id)
        .await?
        .ok_or_else(|| PidoryError::NotFound(format!("session thread_id={thread_id}")))
}

pub async fn get_session_by_thread(
    pool: &PgPool,
    thread_id: &str,
) -> Result<Option<Session>, PidoryError> {
    sqlx::query_as::<_, Session>("SELECT * FROM sessions WHERE thread_id = $1")
        .bind(thread_id)
        .fetch_optional(pool)
        .await
        .map_err(PidoryError::Db)
}

pub async fn update_session_id(
    pool: &PgPool,
    thread_id: &str,
    session_id: &str,
) -> Result<(), PidoryError> {
    sqlx::query("UPDATE sessions SET session_id = $1 WHERE thread_id = $2")
        .bind(session_id)
        .bind(thread_id)
        .execute(pool)
        .await
        .map_err(PidoryError::Db)?;

    Ok(())
}

pub async fn update_session_model(
    pool: &PgPool,
    thread_id: &str,
    model: &str,
) -> Result<(), PidoryError> {
    sqlx::query("UPDATE sessions SET model = $1 WHERE thread_id = $2")
        .bind(model)
        .bind(thread_id)
        .execute(pool)
        .await
        .map_err(PidoryError::Db)?;

    Ok(())
}

pub async fn update_session_status(
    pool: &PgPool,
    thread_id: &str,
    status: &str,
) -> Result<(), PidoryError> {
    sqlx::query(
        "UPDATE sessions SET status = $1, last_active_at = TO_CHAR(NOW(), 'YYYY-MM-DD HH24:MI:SS') WHERE thread_id = $2",
    )
    .bind(status)
    .bind(thread_id)
    .execute(pool)
    .await
    .map_err(PidoryError::Db)?;

    Ok(())
}

pub async fn update_last_active(
    pool: &PgPool,
    thread_id: &str,
) -> Result<(), PidoryError> {
    sqlx::query("UPDATE sessions SET last_active_at = TO_CHAR(NOW(), 'YYYY-MM-DD HH24:MI:SS') WHERE thread_id = $1")
        .bind(thread_id)
        .execute(pool)
        .await
        .map_err(PidoryError::Db)?;

    Ok(())
}

pub async fn list_sessions_by_channel(
    pool: &PgPool,
    channel_id: &str,
) -> Result<Vec<Session>, PidoryError> {
    sqlx::query_as::<_, Session>(
        "SELECT * FROM sessions WHERE channel_id = $1 ORDER BY created_at DESC",
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await
    .map_err(PidoryError::Db)
}

pub async fn delete_session(
    pool: &PgPool,
    thread_id: &str,
) -> Result<(), PidoryError> {
    sqlx::query("DELETE FROM sessions WHERE thread_id = $1")
        .bind(thread_id)
        .execute(pool)
        .await
        .map_err(PidoryError::Db)?;

    Ok(())
}

pub async fn delete_sessions_by_channel(
    pool: &PgPool,
    channel_id: &str,
) -> Result<(), PidoryError> {
    sqlx::query("DELETE FROM sessions WHERE channel_id = $1")
        .bind(channel_id)
        .execute(pool)
        .await
        .map_err(PidoryError::Db)?;

    Ok(())
}

pub async fn try_acquire_session(
    pool: &PgPool,
    thread_id: &str,
) -> Result<bool, PidoryError> {
    let result = sqlx::query(
        "UPDATE sessions SET status = 'running', last_active_at = TO_CHAR(NOW(), 'YYYY-MM-DD HH24:MI:SS') WHERE thread_id = $1 AND status != 'running'"
    )
    .bind(thread_id)
    .execute(pool)
    .await
    .map_err(PidoryError::Db)?;

    Ok(result.rows_affected() > 0)
}

pub async fn reset_running_sessions(pool: &PgPool) -> Result<u64, PidoryError> {
    let result = sqlx::query("UPDATE sessions SET status = 'idle' WHERE status = 'running'")
        .execute(pool)
        .await
        .map_err(PidoryError::Db)?;

    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    async fn setup_db() -> PgPool {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must be set for db integration tests");
        let pool = PgPool::connect(&database_url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("TRUNCATE sessions, projects RESTART IDENTITY CASCADE")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn project_crud() {
        let pool = setup_db().await;

        // register
        let project = register_project(&pool, "ch1", "/tmp/project", Some("test")).await.unwrap();
        assert_eq!(project.channel_id, "ch1");
        assert_eq!(project.path, "/tmp/project");
        assert_eq!(project.name, Some("test".to_string()));

        // get
        let found = get_project_by_channel(&pool, "ch1").await.unwrap();
        assert!(found.is_some());

        // list
        let all = list_projects(&pool).await.unwrap();
        assert_eq!(all.len(), 1);

        // unregister
        unregister_project(&pool, "ch1").await.unwrap();
        let found = get_project_by_channel(&pool, "ch1").await.unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn session_lifecycle() {
        let pool = setup_db().await;

        // project first (FK)
        register_project(&pool, "ch1", "/tmp", None).await.unwrap();

        // create
        let session = create_session(&pool, "th1", "ch1").await.unwrap();
        assert_eq!(session.thread_id, "th1");
        assert_eq!(session.status, "idle");
        assert!(session.session_id.is_none());

        // update session_id
        update_session_id(&pool, "th1", "uuid-123").await.unwrap();
        let s = get_session_by_thread(&pool, "th1").await.unwrap().unwrap();
        assert_eq!(s.session_id, Some("uuid-123".to_string()));

        // update model
        assert!(s.model.is_none());
        update_session_model(&pool, "th1", "claude-opus-4-5").await.unwrap();
        let s = get_session_by_thread(&pool, "th1").await.unwrap().unwrap();
        assert_eq!(s.model, Some("claude-opus-4-5".to_string()));

        // update status
        update_session_status(&pool, "th1", "running").await.unwrap();
        let s = get_session_by_thread(&pool, "th1").await.unwrap().unwrap();
        assert_eq!(s.status, "running");
        assert!(s.last_active_at.is_some());

        // list by channel
        let sessions = list_sessions_by_channel(&pool, "ch1").await.unwrap();
        assert_eq!(sessions.len(), 1);

        // delete
        delete_session(&pool, "th1").await.unwrap();
        let s = get_session_by_thread(&pool, "th1").await.unwrap();
        assert!(s.is_none());
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn reset_running_sessions_test() {
        let pool = setup_db().await;
        register_project(&pool, "ch1", "/tmp", None).await.unwrap();
        create_session(&pool, "th1", "ch1").await.unwrap();
        create_session(&pool, "th2", "ch1").await.unwrap();

        update_session_status(&pool, "th1", "running").await.unwrap();
        update_session_status(&pool, "th2", "running").await.unwrap();

        let count = reset_running_sessions(&pool).await.unwrap();
        assert_eq!(count, 2);

        let s1 = get_session_by_thread(&pool, "th1").await.unwrap().unwrap();
        assert_eq!(s1.status, "idle");
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn duplicate_project_fails() {
        let pool = setup_db().await;
        register_project(&pool, "ch1", "/tmp", None).await.unwrap();
        let result = register_project(&pool, "ch1", "/other", None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn try_acquire_session_test() {
        let pool = setup_db().await;
        register_project(&pool, "ch1", "/tmp", None).await.unwrap();
        create_session(&pool, "th1", "ch1").await.unwrap();

        // First acquire should succeed
        let acquired = try_acquire_session(&pool, "th1").await.unwrap();
        assert!(acquired);

        let s = get_session_by_thread(&pool, "th1").await.unwrap().unwrap();
        assert_eq!(s.status, "running");

        // Second acquire while running should fail
        let acquired_again = try_acquire_session(&pool, "th1").await.unwrap();
        assert!(!acquired_again);
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn get_nonexistent() {
        let pool = setup_db().await;
        let p = get_project_by_channel(&pool, "nonexistent").await.unwrap();
        assert!(p.is_none());
        let s = get_session_by_thread(&pool, "nonexistent").await.unwrap();
        assert!(s.is_none());
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn delete_sessions_by_channel_test() {
        let pool = setup_db().await;

        // Register a project and create 2 sessions
        register_project(&pool, "ch1", "/tmp/proj", None).await.unwrap();
        create_session(&pool, "th1", "ch1").await.unwrap();
        create_session(&pool, "th2", "ch1").await.unwrap();

        // Verify both sessions exist
        let sessions = list_sessions_by_channel(&pool, "ch1").await.unwrap();
        assert_eq!(sessions.len(), 2);

        // Delete all sessions for the channel
        delete_sessions_by_channel(&pool, "ch1").await.unwrap();

        // Both sessions should be deleted
        let s1 = get_session_by_thread(&pool, "th1").await.unwrap();
        assert!(s1.is_none());
        let s2 = get_session_by_thread(&pool, "th2").await.unwrap();
        assert!(s2.is_none());

        // The project should still exist
        let project = get_project_by_channel(&pool, "ch1").await.unwrap();
        assert!(project.is_some());
    }
}
