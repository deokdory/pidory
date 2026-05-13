use poise::ChoiceParameter;

use crate::{Context, Error};
use crate::claude_settings::rule::{Scope, set_default_scope_cache};
use crate::db::repository;

/// `/permissions config default-scope` 의 선택지
#[derive(Debug, Clone, Copy, ChoiceParameter)]
pub enum ScopeChoice {
    #[name = "project"]
    Project,
    #[name = "global"]
    Global,
}

/// Permission 설정 관리
#[poise::command(
    slash_command,
    guild_only,
    owners_only,
    subcommands("config"),
    subcommand_required
)]
pub async fn permissions(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Permission config 서브커맨드 그룹
#[poise::command(
    slash_command,
    guild_only,
    owners_only,
    subcommands("config_default_scope", "config_show"),
    subcommand_required,
    rename = "config"
)]
pub async fn config(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// default-scope 설정 (project | global)
#[poise::command(slash_command, guild_only, owners_only, rename = "default-scope")]
pub async fn config_default_scope(
    ctx: Context<'_>,
    #[description = "Default permission scope"] scope: ScopeChoice,
) -> Result<(), Error> {
    let data = ctx.data();
    let owner_id: i64 = i64::try_from(data.config.discord.owner_id)
        .expect("Discord snowflake fits in i64 (snowflake < 2^63)");

    let new_scope = match scope {
        ScopeChoice::Global => Scope::Global,
        ScopeChoice::Project => Scope::Project,
    };

    // DB upsert 먼저
    repository::upsert_user_default_scope(&data.db, owner_id, new_scope).await?;

    // 성공 시 cache 갱신
    set_default_scope_cache(new_scope);

    let reply = poise::CreateReply::default()
        .content(format!(
            "✅ default-scope → `{}`",
            new_scope.as_str()
        ))
        .ephemeral(true);
    ctx.send(reply).await?;

    Ok(())
}

/// 현재 permission 설정 확인
#[poise::command(slash_command, guild_only, owners_only, rename = "show")]
pub async fn config_show(ctx: Context<'_>) -> Result<(), Error> {
    let data = ctx.data();
    let owner_id: i64 = i64::try_from(data.config.discord.owner_id)
        .expect("Discord snowflake fits in i64 (snowflake < 2^63)");

    let scope_from_db = repository::get_user_default_scope(&data.db, owner_id)
        .await?
        .unwrap_or(Scope::Project);

    let reply = poise::CreateReply::default()
        .content(format!(
            "**Permission config**\ndefault-scope: `{}`",
            scope_from_db.as_str()
        ))
        .ephemeral(true);
    ctx.send(reply).await?;

    Ok(())
}
