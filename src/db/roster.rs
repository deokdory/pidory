use sqlx::{types::Json, PgPool};

use super::models::MemberRoster;
use crate::error::PidoryError;

/// guild member 를 upsert (insert or update) 한다.
pub async fn upsert_member(
    pool: &PgPool,
    guild_id: i64,
    user_id: i64,
    username: &str,
    global_name: Option<&str>,
    guild_nickname: Option<&str>,
    aliases: &[String],
) -> Result<(), PidoryError> {
    sqlx::query(
        "INSERT INTO member_roster (guild_id, user_id, username, global_name, guild_nickname, aliases, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, NOW())
         ON CONFLICT (guild_id, user_id) DO UPDATE SET
             username       = EXCLUDED.username,
             global_name    = EXCLUDED.global_name,
             guild_nickname = EXCLUDED.guild_nickname,
             aliases        = EXCLUDED.aliases,
             updated_at     = NOW()",
    )
    .bind(guild_id)
    .bind(user_id)
    .bind(username)
    .bind(global_name)
    .bind(guild_nickname)
    .bind(Json(aliases))
    .execute(pool)
    .await
    .map_err(PidoryError::Db)?;

    Ok(())
}

/// (guild_id, user_id) 로 단일 멤버를 조회한다.
pub async fn get_member(
    pool: &PgPool,
    guild_id: i64,
    user_id: i64,
) -> Result<Option<MemberRoster>, PidoryError> {
    sqlx::query_as::<_, MemberRoster>(
        "SELECT guild_id, user_id, username, global_name, guild_nickname, aliases, updated_at
         FROM member_roster
         WHERE guild_id = $1 AND user_id = $2",
    )
    .bind(guild_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(PidoryError::Db)
}

/// (guild_id, user_id) 멤버를 삭제한다.
pub async fn delete_member(
    pool: &PgPool,
    guild_id: i64,
    user_id: i64,
) -> Result<(), PidoryError> {
    sqlx::query("DELETE FROM member_roster WHERE guild_id = $1 AND user_id = $2")
        .bind(guild_id)
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(PidoryError::Db)?;

    Ok(())
}

/// guild 의 모든 멤버 목록을 반환한다.
pub async fn list_guild_members(
    pool: &PgPool,
    guild_id: i64,
) -> Result<Vec<MemberRoster>, PidoryError> {
    sqlx::query_as::<_, MemberRoster>(
        "SELECT guild_id, user_id, username, global_name, guild_nickname, aliases, updated_at
         FROM member_roster
         WHERE guild_id = $1
         ORDER BY user_id ASC",
    )
    .bind(guild_id)
    .fetch_all(pool)
    .await
    .map_err(PidoryError::Db)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_db() -> PgPool {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL must be set for db integration tests");
        let pool = PgPool::connect(&database_url).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("TRUNCATE member_roster RESTART IDENTITY CASCADE")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    /// aliases JSONB round-trip: upsert with aliases → get_member → aliases preserved.
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn alias_jsonb_round_trip() {
        let pool = setup_db().await;

        let aliases = vec!["jm".to_string(), "재민".to_string()];
        upsert_member(&pool, 1, 100, "jaemin", Some("JaeMin"), Some("재민닉"), &aliases)
            .await
            .unwrap();

        let row = get_member(&pool, 1, 100).await.unwrap().unwrap();
        assert_eq!(row.username, "jaemin");
        assert_eq!(row.global_name, Some("JaeMin".to_string()));
        assert_eq!(row.guild_nickname, Some("재민닉".to_string()));
        assert_eq!(row.aliases.0, aliases);
    }

    /// upsert twice → second call updates fields including aliases.
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn alias_jsonb_upsert_update() {
        let pool = setup_db().await;

        // initial insert
        upsert_member(&pool, 1, 200, "alice", None, None, &["ali".to_string()])
            .await
            .unwrap();

        // update: new aliases list
        let new_aliases = vec!["ali".to_string(), "alicia".to_string()];
        upsert_member(&pool, 1, 200, "alice", None, None, &new_aliases)
            .await
            .unwrap();

        let row = get_member(&pool, 1, 200).await.unwrap().unwrap();
        assert_eq!(row.aliases.0, new_aliases);
    }

    /// empty aliases round-trip: upsert with [] → get_member → aliases is [].
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn alias_jsonb_empty_round_trip() {
        let pool = setup_db().await;

        upsert_member(&pool, 1, 300, "bob", None, None, &[])
            .await
            .unwrap();

        let row = get_member(&pool, 1, 300).await.unwrap().unwrap();
        assert!(row.aliases.0.is_empty());
    }

    /// list_guild_members returns all members for a guild ordered by user_id.
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn list_guild_members_ordered() {
        let pool = setup_db().await;

        upsert_member(&pool, 2, 30, "charlie", None, None, &[]).await.unwrap();
        upsert_member(&pool, 2, 10, "alice", None, None, &[]).await.unwrap();
        upsert_member(&pool, 2, 20, "bob", None, None, &[]).await.unwrap();

        let rows = list_guild_members(&pool, 2).await.unwrap();
        assert_eq!(rows.len(), 3);
        // ordered by user_id ASC
        assert_eq!(rows[0].user_id, 10);
        assert_eq!(rows[1].user_id, 20);
        assert_eq!(rows[2].user_id, 30);
    }

    /// delete_member removes the member; get_member returns None afterwards.
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn delete_member_test() {
        let pool = setup_db().await;

        upsert_member(&pool, 3, 400, "dave", None, None, &[]).await.unwrap();
        let row = get_member(&pool, 3, 400).await.unwrap();
        assert!(row.is_some());

        delete_member(&pool, 3, 400).await.unwrap();
        let row = get_member(&pool, 3, 400).await.unwrap();
        assert!(row.is_none());
    }
}
