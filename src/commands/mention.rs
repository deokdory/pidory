use poise::serenity_prelude as serenity;
use sqlx::types::Json;

use crate::mention::roster::{nfc, RosterEntry};
use crate::{Context, Error};
use crate::db::models::MemberRoster;

// ─── Conflict detection ───────────────────────────────────────────────────────

/// Outcome of [`check_alias_conflict`].
#[derive(Debug, PartialEq)]
pub(crate) enum AliasConflict {
    /// alias is already registered to a *different* user.
    OtherUserAlias { owner_id: i64 },
    /// alias equals the `username` of some member.
    Username { owner_id: i64 },
    /// alias equals the `global_name` of some member.
    GlobalName { owner_id: i64 },
    /// alias equals the `guild_nickname` of some member.
    GuildNickname { owner_id: i64 },
    /// alias already exists on the *same* user (no-op).
    SelfDuplicate,
    /// No conflict — alias can be added.
    None,
}

/// Pure function: check whether `alias` can be added for `target_user_id` given
/// the current guild roster (`all_members`).
///
/// Mirrors the conflict-check logic in [`add`] without any DB or Discord calls.
/// The check order is intentional — (a) cross-user alias clash, (b) name field
/// clash (username / global_name / guild_nickname), (c) self-duplicate — and
/// matches the handler exactly.
///
/// `self-user` rows are skipped in the cross-user alias check (a) but NOT in the
/// name-field check (b): if target's own username happens to equal the alias,
/// that is still rejected to maintain unambiguous name resolution.
///
/// All comparisons are performed on NFC-normalized strings to match
/// `RosterSnapshot::resolve` behavior (which also NFC-normalizes before comparing).
pub(crate) fn check_alias_conflict(
    all_members: &[MemberRoster],
    target_user_id: i64,
    alias: &str,
) -> AliasConflict {
    // Normalize alias to NFC once — consistent with RosterSnapshot::resolve
    let alias_nfc = nfc(alias);

    // (a) alias registered to a *different* user?
    for member in all_members {
        if member.user_id == target_user_id {
            continue;
        }
        if member.aliases.0.iter().any(|a| nfc(a) == alias_nfc) {
            return AliasConflict::OtherUserAlias { owner_id: member.user_id };
        }
    }

    // (b) alias collides with any member's name fields (all members, incl. self)?
    for member in all_members {
        if nfc(&member.username) == alias_nfc {
            return AliasConflict::Username { owner_id: member.user_id };
        }
        if member.global_name.as_deref().map(nfc).as_deref() == Some(alias_nfc.as_str()) {
            return AliasConflict::GlobalName { owner_id: member.user_id };
        }
        if member.guild_nickname.as_deref().map(nfc).as_deref() == Some(alias_nfc.as_str()) {
            return AliasConflict::GuildNickname { owner_id: member.user_id };
        }
    }

    // (c) same user already has this alias?
    for member in all_members {
        if member.user_id == target_user_id && member.aliases.0.iter().any(|a| nfc(a) == alias_nfc) {
            return AliasConflict::SelfDuplicate;
        }
    }

    AliasConflict::None
}

// ─── Commands ─────────────────────────────────────────────────────────────────

/// Mention 로스터 관리
#[poise::command(
    slash_command,
    guild_only,
    owners_only,
    subcommands("add"),
    subcommand_required
)]
pub async fn mention(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// 유저에게 커스텀 호칭(alias) 추가
#[poise::command(slash_command, guild_only, owners_only)]
pub async fn add(
    ctx: Context<'_>,
    #[description = "대상 유저"] user: serenity::User,
    #[description = "추가할 호칭"] alias: String,
) -> Result<(), Error> {
    let data = ctx.data();

    let guild_id = match ctx.guild_id() {
        Some(id) => id,
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .content("❌ 서버 채널에서만 사용할 수 있어요.")
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
    };

    let guild_id_i64 = guild_id.get() as i64;
    let user_id_i64 = user.id.get() as i64;

    // alias 정규화: 앞뒤 공백 제거 후 NFC 정규화 (resolver와 동일한 canonical form)
    let alias = nfc(alias.trim());

    if alias.is_empty() {
        ctx.send(
            poise::CreateReply::default()
                .content("❌ 호칭이 비어 있어요.")
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    // ── advisory lock + 검증 + 쓰기를 단일 트랜잭션으로 직렬화 ─────────────────
    //
    // pg_advisory_xact_lock(guild_id) 으로 guild 별 alias mutation 을 직렬화한다.
    // lock 은 트랜잭션 종료 시 자동 해제되므로 별도 unlock 불필요.
    let mut tx = data.db.begin().await?;

    // guild_id 기준 advisory lock 획득 (동일 guild 내 동시 add 직렬화)
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(guild_id_i64)
        .execute(&mut *tx)
        .await?;

    // 기존 멤버 row 조회 (tx 안에서)
    let existing: Option<MemberRoster> = sqlx::query_as::<_, MemberRoster>(
        "SELECT guild_id, user_id, username, global_name, guild_nickname, aliases, updated_at
         FROM member_roster
         WHERE guild_id = $1 AND user_id = $2",
    )
    .bind(guild_id_i64)
    .bind(user_id_i64)
    .fetch_optional(&mut *tx)
    .await?;

    // 충돌 검사: 같은 guild 내 모든 멤버 목록 가져오기 (tx 안에서, lock 이후)
    let all_members: Vec<MemberRoster> = sqlx::query_as::<_, MemberRoster>(
        "SELECT guild_id, user_id, username, global_name, guild_nickname, aliases, updated_at
         FROM member_roster
         WHERE guild_id = $1
         ORDER BY user_id ASC",
    )
    .bind(guild_id_i64)
    .fetch_all(&mut *tx)
    .await?;

    match check_alias_conflict(&all_members, user_id_i64, &alias) {
        AliasConflict::OtherUserAlias { owner_id } => {
            tx.rollback().await?;
            ctx.send(
                poise::CreateReply::default()
                    .content(format!(
                        "❌ `{}` 호칭은 이미 <@{}> 에 등록되어 있어요. 중복 등록은 불가능해요.",
                        alias, owner_id as u64
                    ))
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
        AliasConflict::Username { owner_id } => {
            tx.rollback().await?;
            ctx.send(
                poise::CreateReply::default()
                    .content(format!(
                        "❌ `{}` 은 이미 <@{}> 의 username 이에요. 이름 충돌을 방지하기 위해 등록할 수 없어요.",
                        alias, owner_id as u64
                    ))
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
        AliasConflict::GlobalName { owner_id } => {
            tx.rollback().await?;
            ctx.send(
                poise::CreateReply::default()
                    .content(format!(
                        "❌ `{}` 은 이미 <@{}> 의 global name 이에요. 이름 충돌을 방지하기 위해 등록할 수 없어요.",
                        alias, owner_id as u64
                    ))
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
        AliasConflict::GuildNickname { owner_id } => {
            tx.rollback().await?;
            ctx.send(
                poise::CreateReply::default()
                    .content(format!(
                        "❌ `{}` 은 이미 <@{}> 의 서버 닉네임이에요. 이름 충돌을 방지하기 위해 등록할 수 없어요.",
                        alias, owner_id as u64
                    ))
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
        AliasConflict::SelfDuplicate => {
            tx.rollback().await?;
            ctx.send(
                poise::CreateReply::default()
                    .content(format!(
                        "ℹ️ `{}` 호칭은 이미 <@{}> 에 등록되어 있어요.",
                        alias, user.id
                    ))
                    .ephemeral(true),
            )
            .await?;
            return Ok(());
        }
        AliasConflict::None => {}
    }

    // 기존 row 에서 aliases 읽거나, 없으면 새 row 생성
    let (username, global_name, guild_nickname, mut aliases) = match existing {
        Some(member) => {
            (
                member.username,
                member.global_name,
                member.guild_nickname,
                member.aliases.0,
            )
        }
        None => {
            // roster row 없음 → user 정보로 새 row 생성
            let username = user.name.clone();
            let global_name = user.global_name.as_ref().map(|s| s.to_string());
            (username, global_name, None, Vec::new())
        }
    };

    // NFC-canonical alias 저장 (resolve 와 정합)
    aliases.push(alias.clone());

    // DB upsert (tx 안에서 직접 실행 — advisory lock 범위 유지)
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
    .bind(guild_id_i64)
    .bind(user_id_i64)
    .bind(&username)
    .bind(global_name.as_deref())
    .bind(guild_nickname.as_deref())
    .bind(Json(&aliases))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // in-memory cache 갱신
    let new_entry = RosterEntry {
        user_id: user.id,
        username,
        global_name,
        guild_nickname,
        aliases,
    };
    data.roster_cache.upsert_entry(guild_id, new_entry).await;

    ctx.send(
        poise::CreateReply::default()
            .content(format!(
                "✅ <@{}> 에게 `{}` 호칭을 등록했어요.",
                user.id, alias
            ))
            .ephemeral(true),
    )
    .await?;

    Ok(())
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sqlx::types::Json;

    use super::*;
    use crate::db::models::MemberRoster;

    /// Build a minimal MemberRoster for testing — no DB required.
    fn make_member(
        user_id: i64,
        username: &str,
        global_name: Option<&str>,
        guild_nickname: Option<&str>,
        aliases: &[&str],
    ) -> MemberRoster {
        MemberRoster {
            guild_id: 1,
            user_id,
            username: username.to_string(),
            global_name: global_name.map(str::to_string),
            guild_nickname: guild_nickname.map(str::to_string),
            aliases: Json(aliases.iter().map(|s| s.to_string()).collect()),
            updated_at: Utc::now(),
        }
    }

    // ① alias 가 다른 user 에 이미 등록되어 있으면 거부
    #[test]
    fn conflict_other_user_alias() {
        let members = vec![
            make_member(100, "alice", None, None, &["ali"]),
            make_member(200, "bob", None, None, &[]),
        ];
        // target = 200 (bob), alias = "ali" (registered to alice=100)
        let result = check_alias_conflict(&members, 200, "ali");
        assert_eq!(result, AliasConflict::OtherUserAlias { owner_id: 100 });
    }

    // ② alias 가 어떤 멤버의 username 과 동일하면 거부
    #[test]
    fn conflict_username() {
        let members = vec![
            make_member(100, "alice", None, None, &[]),
            make_member(200, "bob", None, None, &[]),
        ];
        // trying to add "alice" as alias for user 200
        let result = check_alias_conflict(&members, 200, "alice");
        assert_eq!(result, AliasConflict::Username { owner_id: 100 });
    }

    // ③ alias 가 어떤 멤버의 global_name 과 동일하면 거부
    #[test]
    fn conflict_global_name() {
        let members = vec![
            make_member(100, "alice", Some("Alice Global"), None, &[]),
            make_member(200, "bob", None, None, &[]),
        ];
        let result = check_alias_conflict(&members, 200, "Alice Global");
        assert_eq!(result, AliasConflict::GlobalName { owner_id: 100 });
    }

    // ④ alias 가 어떤 멤버의 guild_nickname 과 동일하면 거부
    #[test]
    fn conflict_guild_nickname() {
        let members = vec![
            make_member(100, "alice", None, Some("앨리스닉"), &[]),
            make_member(200, "bob", None, None, &[]),
        ];
        let result = check_alias_conflict(&members, 200, "앨리스닉");
        assert_eq!(result, AliasConflict::GuildNickname { owner_id: 100 });
    }

    // ⑤ 같은 user 에 이미 등록된 alias → SelfDuplicate (no-op)
    #[test]
    fn conflict_self_duplicate() {
        let members = vec![
            make_member(100, "alice", None, None, &["ali", "앨리"]),
        ];
        let result = check_alias_conflict(&members, 100, "ali");
        assert_eq!(result, AliasConflict::SelfDuplicate);
    }

    // ⑥ 충돌 없음 → None (허용)
    #[test]
    fn conflict_none() {
        let members = vec![
            make_member(100, "alice", Some("앨리스"), Some("앨리스닉"), &["ali"]),
            make_member(200, "bob", None, None, &["바비"]),
        ];
        // "봅" is not used by anyone
        let result = check_alias_conflict(&members, 200, "봅");
        assert_eq!(result, AliasConflict::None);
    }

    // ⑦ self-user 는 cross-user alias 검사(a)에서 제외 — 본인 기존 alias 는 OtherUserAlias 로 잡히지 않음
    //    (SelfDuplicate 가 올바른 결과)
    #[test]
    fn self_user_excluded_from_cross_user_check() {
        let members = vec![
            make_member(100, "alice", None, None, &["ali"]),
        ];
        // target IS user 100; "ali" belongs to themselves → must be SelfDuplicate, not OtherUserAlias
        let result = check_alias_conflict(&members, 100, "ali");
        assert_eq!(result, AliasConflict::SelfDuplicate);
    }

    // ⑧ NFC 충돌 감지: 분해형(NFD) alias 가 완성형(NFC) alias 와 충돌해야 함
    #[test]
    fn conflict_nfc_decomposed_vs_composed_alias() {
        // "가" NFC (완성형 U+AC00)
        let composed = "\u{AC00}";
        // "가" NFD (분해형: ㄱ U+1100 + ㅏ U+1161)
        let decomposed = "\u{1100}\u{1161}";

        // alice 가 완성형 "가" alias 를 이미 보유
        let members = vec![
            make_member(100, "alice", None, None, &[composed]),
            make_member(200, "bob", None, None, &[]),
        ];
        // bob 이 분해형 "가" alias 추가 시도 → NFC 후 동일 → OtherUserAlias
        let result = check_alias_conflict(&members, 200, decomposed);
        assert_eq!(result, AliasConflict::OtherUserAlias { owner_id: 100 });
    }

    // ⑨ NFC 충돌 감지: 분해형 alias 가 완성형 username 과 충돌해야 함
    #[test]
    fn conflict_nfc_decomposed_alias_vs_composed_username() {
        // alice 의 username 이 완성형 "가나"
        let composed_username = "\u{AC00}\u{B098}"; // "가나"
        // 분해형 "가나"
        let decomposed = "\u{1100}\u{1161}\u{1102}\u{1161}"; // NFD of "가나"

        let members = vec![
            make_member(100, composed_username, None, None, &[]),
            make_member(200, "bob", None, None, &[]),
        ];
        // bob 이 분해형 "가나" alias 추가 시도 → NFC 후 alice username 과 충돌
        let result = check_alias_conflict(&members, 200, decomposed);
        assert_eq!(result, AliasConflict::Username { owner_id: 100 });
    }

    // ⑩ NFC 충돌 감지: SelfDuplicate — 분해형으로 재등록 시도
    #[test]
    fn conflict_nfc_self_duplicate_decomposed() {
        // alice 가 완성형 "나" alias 를 보유
        let composed = "\u{B098}"; // "나"
        let decomposed = "\u{1102}\u{1161}"; // NFD of "나"

        let members = vec![
            make_member(100, "alice", None, None, &[composed]),
        ];
        // 분해형으로 동일 alias 재등록 시도 → SelfDuplicate
        let result = check_alias_conflict(&members, 100, decomposed);
        assert_eq!(result, AliasConflict::SelfDuplicate);
    }
}
