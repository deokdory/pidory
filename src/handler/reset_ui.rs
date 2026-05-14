use poise::serenity_prelude::{
    ButtonStyle, ChannelId, Context, CreateActionRow, CreateButton, CreateMessage, EditMessage,
    MessageId, UserId,
};
use crate::i18n::Lang;

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

#[allow(dead_code)]
pub fn create_reset_confirm_message(content: &str, thread_id: &str, lang: Lang) -> CreateMessage {
    let confirm_btn = CreateButton::new(format!("reset:{}:confirm", thread_id))
        .label(lang.btn_reset_confirm())
        .style(ButtonStyle::Danger);
    let cancel_btn = CreateButton::new(format!("reset:{}:cancel", thread_id))
        .label(lang.btn_reset_cancel())
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
    lang: Lang,
) -> Result<(), serenity::Error> {
    let label = match outcome {
        ResetOutcome::Confirmed => lang.reset_done(),
        ResetOutcome::Cancelled => lang.reset_cancelled_label(),
        ResetOutcome::Expired => lang.reset_expired_label(),
    };

    let edit = EditMessage::new().content(label).components(vec![]);

    channel_id.edit_message(ctx, message_id, edit).await?;

    Ok(())
}

