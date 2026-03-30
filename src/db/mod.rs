pub mod models;
pub mod repository;

use sqlx::SqlitePool;

use crate::error::PidoryError;

pub async fn init_pool(db_path: &str) -> Result<SqlitePool, PidoryError> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&format!("sqlite://{}?mode=rwc", db_path))
        .await
        .map_err(PidoryError::Db)?;

    // WAL mode 활성화
    sqlx::query("PRAGMA journal_mode=WAL")
        .execute(&pool)
        .await
        .map_err(PidoryError::Db)?;

    // 마이그레이션 실행
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|e| PidoryError::Db(e.into()))?;

    Ok(pool)
}
