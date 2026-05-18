use sqlx::PgPool;

use super::models::MemberRoster;
use crate::error::PidoryError;

/// guild member 를 upsert 하되 aliases 컬럼은 건드리지 않는다.
///
/// gateway 이벤트(GuildMemberAdd / GuildMemberUpdate) 전용.
/// INSERT 시에만 aliases 가 DEFAULT `'[]'` 로 초기화되고,
/// ON CONFLICT DO UPDATE 에서 aliases 는 기존 값을 그대로 유지한다.
pub async fn upsert_member_preserve_aliases(
    pool: &PgPool,
    guild_id: i64,
    user_id: i64,
    username: &str,
    global_name: Option<&str>,
    guild_nickname: Option<&str>,
) -> Result<(), PidoryError> {
    sqlx::query(
        "INSERT INTO member_roster (guild_id, user_id, username, global_name, guild_nickname, aliases, updated_at)
         VALUES ($1, $2, $3, $4, $5, '[]'::jsonb, NOW())
         ON CONFLICT (guild_id, user_id) DO UPDATE SET
             username       = EXCLUDED.username,
             global_name    = EXCLUDED.global_name,
             guild_nickname = EXCLUDED.guild_nickname,
             updated_at     = NOW()",
    )
    .bind(guild_id)
    .bind(user_id)
    .bind(username)
    .bind(global_name)
    .bind(guild_nickname)
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

    /// list_guild_members returns all members for a guild ordered by user_id.
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn list_guild_members_ordered() {
        let pool = setup_db().await;

        upsert_member_preserve_aliases(&pool, 2, 30, "charlie", None, None).await.unwrap();
        upsert_member_preserve_aliases(&pool, 2, 10, "alice", None, None).await.unwrap();
        upsert_member_preserve_aliases(&pool, 2, 20, "bob", None, None).await.unwrap();

        let rows = list_guild_members(&pool, 2).await.unwrap();
        assert_eq!(rows.len(), 3);
        // ordered by user_id ASC
        assert_eq!(rows[0].user_id, 10);
        assert_eq!(rows[1].user_id, 20);
        assert_eq!(rows[2].user_id, 30);
    }

    /// upsert_member_preserve_aliases: 기존 aliases 보존하며 username 등만 갱신.
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn preserve_aliases_on_gateway_upsert() {
        let pool = setup_db().await;

        // 초기 insert: aliases 없이 삽입 후 직접 SQL로 aliases 설정
        upsert_member_preserve_aliases(&pool, 10, 1001, "alice", Some("Alice"), None)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE member_roster SET aliases = $1::jsonb WHERE guild_id = $2 AND user_id = $3",
        )
        .bind(r#"["별명A","별명B"]"#)
        .bind(10_i64)
        .bind(1001_i64)
        .execute(&pool)
        .await
        .unwrap();

        // gateway upsert: username/guild_nickname 변경, aliases 미전달
        upsert_member_preserve_aliases(&pool, 10, 1001, "alice_new", Some("AliceNew"), Some("앨리스"))
            .await
            .unwrap();

        let row = get_member(&pool, 10, 1001).await.unwrap().unwrap();
        // username/guild_nickname 갱신 확인
        assert_eq!(row.username, "alice_new");
        assert_eq!(row.global_name, Some("AliceNew".to_string()));
        assert_eq!(row.guild_nickname, Some("앨리스".to_string()));
        // aliases 보존 확인 — 덮어쓰지 않음
        let expected = vec!["별명A".to_string(), "별명B".to_string()];
        assert_eq!(row.aliases.0, expected);
    }

    /// upsert_member_preserve_aliases: 신규 INSERT 시 aliases 는 빈 배열.
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn preserve_aliases_new_member_empty_aliases() {
        let pool = setup_db().await;

        upsert_member_preserve_aliases(&pool, 10, 2001, "bob", None, None)
            .await
            .unwrap();

        let row = get_member(&pool, 10, 2001).await.unwrap().unwrap();
        assert_eq!(row.username, "bob");
        assert!(row.aliases.0.is_empty());
    }

    /// delete_member removes the member; get_member returns None afterwards.
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn delete_member_test() {
        let pool = setup_db().await;

        upsert_member_preserve_aliases(&pool, 3, 400, "dave", None, None).await.unwrap();
        let row = get_member(&pool, 3, 400).await.unwrap();
        assert!(row.is_some());

        delete_member(&pool, 3, 400).await.unwrap();
        let row = get_member(&pool, 3, 400).await.unwrap();
        assert!(row.is_none());
    }
}
