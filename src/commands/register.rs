use std::path::Path;

use poise::serenity_prelude as serenity;

use crate::{Context, Error};
use crate::db::repository;

#[poise::command(slash_command, guild_only, owners_only)]
pub async fn register(
    ctx: Context<'_>,
    #[description = "Project directory path"] path: String,
    #[description = "Display name (optional)"] name: Option<String>,
) -> Result<(), Error> {
    let channel_id = ctx.channel_id().to_string();

    if !Path::new(&path).exists() {
        let reply = poise::CreateReply::default()
            .content(format!("❌ Path does not exist: `{path}`"))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    if let Some(existing) = repository::get_project_by_channel(&ctx.data().db, &channel_id).await? {
        let reply = poise::CreateReply::default()
            .content(format!(
                "❌ This channel is already registered to `{}`. Use `/unregister` first.",
                existing.path
            ))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    repository::register_project(&ctx.data().db, &channel_id, &path, name.as_deref()).await?;

    let reply = poise::CreateReply::default()
        .content(format!("✅ Registered `{path}` to this channel"))
        .ephemeral(true);
    ctx.send(reply).await?;

    // Try to update channel topic; ignore failure
    let _ = ctx
        .channel_id()
        .edit(ctx.http(), serenity::EditChannel::new().topic(format!("pidory: {path}")))
        .await;

    Ok(())
}

#[poise::command(slash_command, guild_only, owners_only)]
pub async fn unregister(ctx: Context<'_>) -> Result<(), Error> {
    let channel_id = ctx.channel_id().to_string();

    let project =
        repository::get_project_by_channel(&ctx.data().db, &channel_id).await?;

    if project.is_none() {
        let reply = poise::CreateReply::default()
            .content("❌ No project registered to this channel")
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    repository::unregister_project(&ctx.data().db, &channel_id).await?;

    let reply = poise::CreateReply::default()
        .content("✅ Unregistered project from this channel")
        .ephemeral(true);
    ctx.send(reply).await?;

    Ok(())
}
