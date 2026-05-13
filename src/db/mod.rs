pub mod models;
pub mod repository;

use sqlx::PgPool;

use crate::error::PidoryError;

pub async fn init_pool(database_url: &str) -> Result<PgPool, PidoryError> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .map_err(PidoryError::Db)?;

    // 마이그레이션 실행
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(|e| PidoryError::Db(e.into()))?;

    Ok(pool)
}
