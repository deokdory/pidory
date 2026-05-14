use poise::serenity_prelude::{Context, ChannelId, MessageId, ReactionType};
use tracing::warn;

use crate::error::PidoryError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionStatus {
    Running,
    Done,
    Error,
    #[allow(dead_code)]
    Timeout,
    Interrupted,
    Downloading,
    InjectQueued,
    QueueFull,
}

impl ReactionStatus {
    pub fn emoji(&self) -> &'static str {
        match self {
            ReactionStatus::Running => "🔄",
            ReactionStatus::Done => "✅",
            ReactionStatus::Error => "❌",
            ReactionStatus::Timeout => "⏰",
            ReactionStatus::Interrupted => "⛔",
            ReactionStatus::Downloading => "⏬",
            ReactionStatus::InjectQueued => "📨",
            ReactionStatus::QueueFull => "⏳",
        }
    }
}

const ALL_EMOJIS: &[&str] = &["🔄", "✅", "❌", "⏰", "📨", "⛔", "⏬", "⏳"];

pub async fn set_reaction(
    ctx: &Context,
    channel_id: ChannelId,
    message_id: MessageId,
    status: ReactionStatus,
) -> Result<(), PidoryError> {
    if let Err(e) = clear_bot_reactions(ctx, channel_id, message_id).await {
        warn!(
            channel_id = %channel_id,
            message_id = %message_id,
            status = ?status,
            "set_reaction failed: {e}"
        );
        return Err(e);
    }

    let reaction = ReactionType::Unicode(status.emoji().to_string());
    if let Err(e) = channel_id
        .create_reaction(ctx, message_id, reaction)
        .await
        .map_err(PidoryError::from)
    {
        warn!(
            channel_id = %channel_id,
            message_id = %message_id,
            status = ?status,
            "set_reaction failed: {e}"
        );
        return Err(e);
    }

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

#[cfg(test)]
mod tests {
    use super::{ALL_EMOJIS, ReactionStatus};

    #[test]
    fn inject_queued_emoji() {
        assert_eq!(ReactionStatus::InjectQueued.emoji(), "📨");
    }

    #[test]
    fn queue_full_emoji() {
        assert_eq!(ReactionStatus::QueueFull.emoji(), "⏳");
    }

    #[test]
    fn existing_variants_unchanged() {
        assert_eq!(ReactionStatus::Running.emoji(), "🔄");
        assert_eq!(ReactionStatus::Done.emoji(), "✅");
        assert_eq!(ReactionStatus::Error.emoji(), "❌");
        assert_eq!(ReactionStatus::Timeout.emoji(), "⏰");
        assert_eq!(ReactionStatus::Interrupted.emoji(), "⛔");
        assert_eq!(ReactionStatus::Downloading.emoji(), "⏬");
    }

    #[test]
    fn all_emojis_contains_every_variant_emoji() {
        let variants = [
            ReactionStatus::Running,
            ReactionStatus::Done,
            ReactionStatus::Error,
            ReactionStatus::Timeout,
            ReactionStatus::Interrupted,
            ReactionStatus::Downloading,
            ReactionStatus::InjectQueued,
            ReactionStatus::QueueFull,
        ];
        for variant in &variants {
            let e = variant.emoji();
            assert!(
                ALL_EMOJIS.contains(&e),
                "ALL_EMOJIS missing emoji {e:?} for variant {variant:?}"
            );
        }
    }
}
