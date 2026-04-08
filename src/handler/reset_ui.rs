use poise::serenity_prelude::{
    ButtonStyle, ChannelId, Context, CreateActionRow, CreateButton, CreateMessage, EditMessage,
    MessageId, UserId,
};

pub enum ResetAction {
    Confirm,
    Cancel,
}

pub enum ResetOutcome {
    Confirmed,
    Cancelled,
    Expired,
}

pub struct PendingReset {
    pub message_id: MessageId,
    pub thread_id: String,
    pub requested_by: UserId,
}

pub fn create_reset_confirm_message(content: &str, thread_id: &str) -> CreateMessage {
    let confirm_btn = CreateButton::new(format!("reset:{}:confirm", thread_id))
        .label("✅ 예, 리셋")
        .style(ButtonStyle::Danger);
    let cancel_btn = CreateButton::new(format!("reset:{}:cancel", thread_id))
        .label("❌ 아니요")
        .style(ButtonStyle::Secondary);

    let row = CreateActionRow::Buttons(vec![confirm_btn, cancel_btn]);

    CreateMessage::new()
        .content(content)
        .components(vec![row])
}

/// Parses custom_id in the format `reset:{thread_id}:{action}`.
/// Returns `(thread_id, ResetAction)` or `None` if the format does not match.
pub fn parse_reset_custom_id(custom_id: &str) -> Option<(String, ResetAction)> {
    let stripped = custom_id.strip_prefix("reset:")?;
    let (thread_id, action) = stripped.rsplit_once(':')?;
    let reset_action = match action {
        "confirm" => ResetAction::Confirm,
        "cancel" => ResetAction::Cancel,
        _ => return None,
    };
    Some((thread_id.to_string(), reset_action))
}

pub async fn disable_reset_buttons(
    ctx: &Context,
    channel_id: ChannelId,
    message_id: MessageId,
    outcome: ResetOutcome,
) -> Result<(), serenity::Error> {
    let label = match outcome {
        ResetOutcome::Confirmed => "✅ 리셋됨",
        ResetOutcome::Cancelled => "❌ 취소됨",
        ResetOutcome::Expired => "⏰ 만료됨",
    };

    let edit = EditMessage::new().content(label).components(vec![]);

    channel_id.edit_message(ctx, message_id, edit).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_confirm() {
        let (tid, action) = parse_reset_custom_id("reset:123:confirm").unwrap();
        assert_eq!(tid, "123");
        assert!(matches!(action, ResetAction::Confirm));
    }

    #[test]
    fn parse_cancel() {
        let (tid, action) = parse_reset_custom_id("reset:123:cancel").unwrap();
        assert_eq!(tid, "123");
        assert!(matches!(action, ResetAction::Cancel));
    }

    #[test]
    fn parse_wrong_prefix() {
        assert!(parse_reset_custom_id("perm:abc:allow").is_none());
    }

    #[test]
    fn parse_unknown_action() {
        assert!(parse_reset_custom_id("reset:123:deny").is_none());
    }

    #[test]
    fn parse_empty_string() {
        assert!(parse_reset_custom_id("").is_none());
    }

    #[test]
    fn parse_no_action() {
        assert!(parse_reset_custom_id("reset:123").is_none());
    }
}
