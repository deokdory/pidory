use poise::serenity_prelude::{
    ButtonStyle, ChannelId, Context, CreateActionRow, CreateButton, CreateMessage, EditMessage,
    MessageId, UserId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

