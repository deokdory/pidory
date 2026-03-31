use poise::serenity_prelude::{Context, ChannelId, MessageId, ReactionType};

use crate::error::PidoryError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionStatus {
    Running,
    Done,
    Error,
    #[allow(dead_code)]
    Timeout,
    Interrupted,
}

impl ReactionStatus {
    pub fn emoji(&self) -> &'static str {
        match self {
            ReactionStatus::Running => "🔄",
            ReactionStatus::Done => "✅",
            ReactionStatus::Error => "❌",
            ReactionStatus::Timeout => "⏰",
            ReactionStatus::Interrupted => "⛔",
        }
    }
}

const ALL_EMOJIS: &[&str] = &["🔄", "✅", "❌", "⏰", "📨", "⛔"];

pub async fn set_reaction(
    ctx: &Context,
    channel_id: ChannelId,
    message_id: MessageId,
    status: ReactionStatus,
) -> Result<(), PidoryError> {
    clear_bot_reactions(ctx, channel_id, message_id).await?;

    let reaction = ReactionType::Unicode(status.emoji().to_string());
    channel_id
        .create_reaction(ctx, message_id, reaction)
        .await
        .map_err(PidoryError::from)?;

    Ok(())
}

pub async fn clear_bot_reactions(
    ctx: &Context,
    channel_id: ChannelId,
    message_id: MessageId,
) -> Result<(), PidoryError> {
    for emoji in ALL_EMOJIS {
        let reaction = ReactionType::Unicode(emoji.to_string());
        // ignore errors — reaction may not exist
        let _ = channel_id
            .delete_reaction_emoji(ctx, message_id, reaction)
            .await;
    }

    Ok(())
}
