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
    let lang = ctx.data().config.language;

    if !Path::new(&path).exists() {
        let reply = poise::CreateReply::default()
            .content(format!("❌ {}", lang.path_not_exist(&path)))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    if let Some(existing) = repository::get_project_by_channel(&ctx.data().db, &channel_id).await? {
        let reply = poise::CreateReply::default()
            .content(format!("❌ {}", lang.already_registered(&existing.path)))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    repository::register_project(&ctx.data().db, &channel_id, &path, name.as_deref()).await?;

    let reply = poise::CreateReply::default()
        .content(format!("✅ {}", lang.registered(&path)))
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
    let lang = ctx.data().config.language;

    let project =
        repository::get_project_by_channel(&ctx.data().db, &channel_id).await?;

    if project.is_none() {
        let reply = poise::CreateReply::default()
            .content(format!("❌ {}", lang.not_registered()))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    // Clean up all sessions for this channel before removing the project
    let sessions = repository::list_sessions_by_channel(&ctx.data().db, &channel_id).await?;
    for session in &sessions {
        if let Err(e) = ctx.data().sessions.kill_session(&session.thread_id).await {
            tracing::warn!(thread_id = %session.thread_id, "Failed to kill session during unregister: {}", e);
        }
        ctx.data().session_skills.lock().await.remove(&session.thread_id);
        ctx.data().permission_rxs.lock().await.remove(&session.thread_id);
    }
    repository::delete_sessions_by_channel(&ctx.data().db, &channel_id).await?;

    repository::unregister_project(&ctx.data().db, &channel_id).await?;

    let reply = poise::CreateReply::default()
        .content(format!("✅ {}", lang.unregistered()))
        .ephemeral(true);
    ctx.send(reply).await?;

    Ok(())
}
