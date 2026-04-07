use poise::serenity_prelude as serenity;

use crate::handler::emoji::{self, ReactionStatus};
use crate::{Context, Error};

/// Recall a queued message before it reaches Claude CLI
#[poise::command(
    context_menu_command = "Recall",
    guild_only,
    owners_only,
)]
pub async fn recall(
    ctx: Context<'_>,
    #[description = "Message to recall"] msg: serenity::Message,
) -> Result<(), Error> {
    let data = ctx.data();
    let lang = data.config.language;
    let thread_id = msg.channel_id.to_string();

    // 세션 존재 확인
    if !data.sessions.session_exists(&thread_id).await {
        ctx.send(
            poise::CreateReply::default()
                .ephemeral(true)
                .content(lang.recall_no_session()),
        )
        .await?;
        return Ok(());
    }

    // 회수 시도
    if data.sessions.try_recall(msg.id).await {
        // 성공: ephemeral 응답 + interrupted 리액션
        ctx.send(
            poise::CreateReply::default()
                .ephemeral(true)
                .content(lang.recall_success()),
        )
        .await?;
        emoji::set_reaction(
            ctx.serenity_context(),
            msg.channel_id,
            msg.id,
            ReactionStatus::Interrupted,
        )
        .await
        .ok();
    } else {
        // 실패: 이미 전달됨
        ctx.send(
            poise::CreateReply::default()
                .ephemeral(true)
                .content(lang.recall_already_sent()),
        )
        .await?;
    }

    Ok(())
}
