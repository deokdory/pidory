use std::collections::HashMap;

use poise::serenity_prelude::{Context, UserId};

use crate::Data;
use crate::db::repository;
use crate::error::PidoryError;
use crate::handler::{permission_ui, question_ui, reset_ui};
use crate::i18n::Lang;
use crate::subprocess::permission::PermissionDecision;

/// Verifies the interacting user is the triggering user or the bot owner.
/// Returns `Some(triggered_by)` if authorized, `None` if rejected (ephemeral sent).
pub(super) async fn verify_component_auth(
    component: &poise::serenity_prelude::ComponentInteraction,
    ctx: &Context,
    data: &Data,
    request_id: &str,
    lang: Lang,
) -> Option<UserId> {
    let triggered_by = {
        let pending = data.pending_permissions.lock().await;
        pending.get(request_id).map(|p| p.triggered_by)
    };

    let triggered_by = triggered_by?;

    let is_owner = component.user.id == UserId::new(data.config.discord.owner_id);
    if component.user.id != triggered_by && !is_owner {
        component
            .create_response(
                ctx,
                poise::serenity_prelude::CreateInteractionResponse::Message(
                    poise::serenity_prelude::CreateInteractionResponseMessage::new()
                        .content(format!("❌ {}", lang.no_permission()))
                        .ephemeral(true),
                ),
            )
            .await
            .ok();
        return None;
    }

    Some(triggered_by)
}

/// Handles a question answer — either direct (single question) or group (multi-question).
///
/// For single questions, the caller has already removed the PendingPermission and passes
/// its `response_tx` directly. For multi-question groups, the answer is stored in the
/// PendingQuestionGroup; when all answers are collected, the group's `response_tx` fires.
pub(super) async fn handle_question_answer(
    data: &Data,
    request_id: &str,
    answer: String,
    question_index: usize,
    response_tx: tokio::sync::oneshot::Sender<PermissionDecision>,
) {
    if let Some((group_id, q_idx)) = question_ui::parse_sub_request_id(request_id) {
        // Multi-question group member — store answer in group
        // The caller's response_tx is a dummy; drop it and use the group's instead.
        drop(response_tx);
        let mut groups = data.pending_question_groups.lock().await;
        if let Some(group) = groups.get_mut(&group_id) {
            let key = format!("q_{}", q_idx);
            group.answers.insert(key, answer);
            if group.answers.len() >= group.total {
                let group = groups.remove(&group_id).unwrap();
                let _ = group.response_tx.send(PermissionDecision::Answer(group.answers));
            }
        } else {
            tracing::warn!("PendingQuestionGroup not found for group_id={}", group_id);
        }
    } else {
        // Single question — send directly via the caller's response_tx
        let answers = HashMap::from([(format!("q_{}", question_index), answer)]);
        let _ = response_tx.send(PermissionDecision::Answer(answers));
    }
}

pub(super) async fn handle_interaction(
    ctx: &Context,
    interaction: &poise::serenity_prelude::Interaction,
    data: &Data,
) -> Result<(), PidoryError> {
    match interaction {
        poise::serenity_prelude::Interaction::Modal(modal) => {
            return handle_modal_interaction(ctx, modal, data).await;
        }
        poise::serenity_prelude::Interaction::Component(_) => {}
        _ => return Ok(()),
    }

    let component = match interaction {
        poise::serenity_prelude::Interaction::Component(c) => c,
        _ => return Ok(()),
    };

    let lang = data.config.language;

    // Try permission button first
    if let Some((request_id, action)) =
        permission_ui::parse_permission_custom_id(&component.data.custom_id)
    {
        let Some(_triggered_by) = verify_component_auth(component, ctx, data, &request_id, lang).await else {
            return Ok(());
        };

        // interaction defer — 메시지 업데이트로 응답 (3초 제약)
        component
            .create_response(
                ctx,
                poise::serenity_prelude::CreateInteractionResponse::UpdateMessage(
                    poise::serenity_prelude::CreateInteractionResponseMessage::new(),
                ),
            )
            .await
            .ok();

        let decision = match action.as_str() {
            "allow" => PermissionDecision::Allow,
            "always" => PermissionDecision::AlwaysAllow,
            "deny" => PermissionDecision::Deny,
            _ => return Ok(()),
        };

        // pending_permissions에서 꺼내서 oneshot 전송
        let pending = data
            .pending_permissions
            .lock()
            .await
            .remove(&request_id);

        if let Some(p) = pending {
            let tool_name = p.tool_name.clone();
            let message_id = p.message_id;
            // decision 전송 (실패해도 무시)
            let _ = p.response_tx.send(decision);

            // 버튼 disable
            permission_ui::disable_permission_buttons(
                ctx,
                component.channel_id,
                message_id,
                &action,
                &tool_name,
                lang,
            )
            .await
            .ok();
        }

        return Ok(());
    }

    // Try question option button: ask:{request_id}:{index}
    if let Some((request_id, option_index)) =
        question_ui::parse_question_button_id(&component.data.custom_id)
    {
        let Some(_triggered_by) = verify_component_auth(component, ctx, data, &request_id, lang).await else {
            return Ok(());
        };

        component
            .create_response(
                ctx,
                poise::serenity_prelude::CreateInteractionResponse::UpdateMessage(
                    poise::serenity_prelude::CreateInteractionResponseMessage::new(),
                ),
            )
            .await
            .ok();

        let pending = data.pending_permissions.lock().await.remove(&request_id);
        if let Some(p) = pending {
            let message_id = p.message_id;
            let question_index = question_ui::parse_sub_request_id(&request_id)
                .map(|(_, idx)| idx)
                .unwrap_or(0);
            let input = p.input.unwrap_or_default();
            let label = question_ui::resolve_option_label(
                &input,
                question_index,
                option_index,
            );
            handle_question_answer(data, &request_id, label.clone(), question_index, p.response_tx).await;
            question_ui::disable_question_components(
                ctx,
                component.channel_id,
                message_id,
                &label,
                lang,
            )
            .await
            .ok();
        }

        return Ok(());
    }

    // Try question free-text button: ask_text:{request_id}
    if let Some(request_id) =
        question_ui::parse_question_text_button_id(&component.data.custom_id)
    {
        let Some(_triggered_by) = verify_component_auth(component, ctx, data, &request_id, lang).await else {
            return Ok(());
        };

        // Respond with modal (do NOT defer with UpdateMessage)
        component
            .create_response(
                ctx,
                poise::serenity_prelude::CreateInteractionResponse::Modal(
                    question_ui::create_question_modal(&request_id, lang),
                ),
            )
            .await
            .ok();

        return Ok(());
    }

    // Try question select menu: ask_sel:{request_id}
    if let Some(request_id) = question_ui::parse_question_select_id(&component.data.custom_id) {
        let Some(_triggered_by) = verify_component_auth(component, ctx, data, &request_id, lang).await else {
            return Ok(());
        };

        component
            .create_response(
                ctx,
                poise::serenity_prelude::CreateInteractionResponse::UpdateMessage(
                    poise::serenity_prelude::CreateInteractionResponseMessage::new(),
                ),
            )
            .await
            .ok();

        let selected_index: usize = match &component.data.kind {
            poise::serenity_prelude::ComponentInteractionDataKind::StringSelect { values } => {
                values.first().and_then(|v| v.parse().ok()).unwrap_or(0)
            }
            _ => 0,
        };

        let pending = data.pending_permissions.lock().await.remove(&request_id);
        if let Some(p) = pending {
            let message_id = p.message_id;
            let question_index = question_ui::parse_sub_request_id(&request_id)
                .map(|(_, idx)| idx)
                .unwrap_or(0);
            let input = p.input.unwrap_or_default();
            let label = question_ui::resolve_option_label(
                &input,
                question_index,
                selected_index,
            );
            handle_question_answer(data, &request_id, label.clone(), question_index, p.response_tx).await;
            question_ui::disable_question_components(
                ctx,
                component.channel_id,
                message_id,
                &label,
                lang,
            )
            .await
            .ok();
        }

        return Ok(());
    }

    // Try reset confirm/cancel button: reset:{thread_id}:{action}
    if let Some((thread_id, reset_action)) =
        reset_ui::parse_reset_custom_id(&component.data.custom_id)
    {
        let channel_id = component.channel_id;

        // Authorization: only the requester (or owner) may act
        let requested_by = {
            let pending = data.pending_resets.lock().await;
            pending.get(&thread_id).map(|p| p.requested_by)
        };

        if let Some(requested_by) = requested_by {
            let is_owner = component.user.id == UserId::new(data.config.discord.owner_id);
            if component.user.id != requested_by && !is_owner {
                component
                    .create_response(
                        ctx,
                        poise::serenity_prelude::CreateInteractionResponse::Message(
                            poise::serenity_prelude::CreateInteractionResponseMessage::new()
                                .content(format!("❌ {}", lang.no_permission()))
                                .ephemeral(true),
                        ),
                    )
                    .await
                    .ok();
                return Ok(());
            }
        }

        // Defer the interaction so the button click is acknowledged
        component
            .create_response(
                ctx,
                poise::serenity_prelude::CreateInteractionResponse::Acknowledge,
            )
            .await
            .ok();

        match reset_action {
            reset_ui::ResetAction::Confirm => {
                // Remove from pending — if gone, it expired
                let pending = data.pending_resets.lock().await.remove(&thread_id);
                let Some(pending) = pending else {
                    component
                        .create_followup(
                            ctx,
                            poise::serenity_prelude::CreateInteractionResponseFollowup::new()
                                .content(lang.session_reset_expired())
                                .ephemeral(true),
                        )
                        .await
                        .ok();
                    reset_ui::disable_reset_buttons(
                        ctx,
                        channel_id,
                        // We don't have message_id anymore; best-effort, ignore error
                        component.message.id,
                        reset_ui::ResetOutcome::Expired,
                    )
                    .await
                    .ok();
                    return Ok(());
                };

                // Interrupt the session (ignore failure — may already be idle)
                let _ = data.sessions.interrupt_session(&thread_id).await;

                // Kill the session
                match data.sessions.kill_session(&thread_id).await {
                    Ok(()) | Err(PidoryError::NotFound(_)) => {}
                    Err(e) => {
                        channel_id
                            .say(ctx, format!("❌ {}", lang.error_with(&e)))
                            .await
                            .ok();
                        return Ok(());
                    }
                }

                // Clean up in-memory maps
                data.session_skills.lock().await.remove(&thread_id);
                data.pending_permissions
                    .lock()
                    .await
                    .retain(|_, p| p.thread_id != thread_id);
                data.pending_question_groups
                    .lock()
                    .await
                    .retain(|_, g| g.thread_id != thread_id);
                data.needs_context.lock().await.remove(&thread_id);
                data.turn_initiators.lock().await.remove(&thread_id);
                data.turn_participants.lock().await.remove(&thread_id);
                if let Some(tracker) = data.todo_trackers.lock().await.remove(&thread_id) {
                    tracker.lock().await.cleanup(ctx).await;
                }

                // Remove from DB (best-effort)
                let _ = repository::delete_session(&data.db, &thread_id).await;

                // Disable the confirm/cancel buttons and post result message
                reset_ui::disable_reset_buttons(
                    ctx,
                    channel_id,
                    pending.message_id,
                    reset_ui::ResetOutcome::Confirmed,
                )
                .await
                .ok();

                channel_id
                    .say(ctx, format!("-# ♻️ {}", lang.session_reset()))
                    .await
                    .ok();
            }
            reset_ui::ResetAction::Cancel => {
                let pending = data.pending_resets.lock().await.remove(&thread_id);
                let Some(pending) = pending else {
                    component
                        .create_followup(
                            ctx,
                            poise::serenity_prelude::CreateInteractionResponseFollowup::new()
                                .content(lang.session_reset_expired())
                                .ephemeral(true),
                        )
                        .await
                        .ok();
                    return Ok(());
                };

                reset_ui::disable_reset_buttons(
                    ctx,
                    channel_id,
                    pending.message_id,
                    reset_ui::ResetOutcome::Cancelled,
                )
                .await
                .ok();
            }
        }

        return Ok(());
    }

    Ok(())
}

pub(super) async fn handle_modal_interaction(
    ctx: &Context,
    modal: &poise::serenity_prelude::ModalInteraction,
    data: &Data,
) -> Result<(), PidoryError> {
    let request_id = match question_ui::parse_question_modal_id(&modal.data.custom_id) {
        Some(rid) => rid,
        None => return Ok(()),
    };

    let lang = data.config.language;

    // Authorization check
    let triggered_by = {
        let pending = data.pending_permissions.lock().await;
        pending.get(&request_id).map(|p| p.triggered_by)
    };

    let Some(triggered_by) = triggered_by else {
        return Ok(());
    };

    let is_owner = modal.user.id == UserId::new(data.config.discord.owner_id);
    if modal.user.id != triggered_by && !is_owner {
        modal
            .create_response(
                ctx,
                poise::serenity_prelude::CreateInteractionResponse::Message(
                    poise::serenity_prelude::CreateInteractionResponseMessage::new()
                        .content(format!("❌ {}", lang.no_permission()))
                        .ephemeral(true),
                ),
            )
            .await
            .ok();
        return Ok(());
    }

    // Extract answer from modal input
    let answer = modal
        .data
        .components
        .first()
        .and_then(|row| row.components.first())
        .and_then(|comp| {
            if let poise::serenity_prelude::ActionRowComponent::InputText(input) = comp {
                input.value.clone()
            } else {
                None
            }
        })
        .unwrap_or_default();

    // Acknowledge modal (type 6: DEFERRED_UPDATE_MESSAGE)
    modal
        .create_response(
            ctx,
            poise::serenity_prelude::CreateInteractionResponse::Acknowledge,
        )
        .await
        .ok();

    // Send answer to pending
    let pending = data.pending_permissions.lock().await.remove(&request_id);
    if let Some(p) = pending {
        let message_id = p.message_id;
        let question_index = question_ui::parse_sub_request_id(&request_id)
            .map(|(_, idx)| idx)
            .unwrap_or(0);
        handle_question_answer(data, &request_id, answer.clone(), question_index, p.response_tx).await;
        question_ui::disable_question_components(
            ctx,
            modal.channel_id,
            message_id,
            &answer,
            lang,
        )
        .await
        .ok();
    }

    Ok(())
}
