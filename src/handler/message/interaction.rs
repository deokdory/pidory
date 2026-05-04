use std::collections::HashMap;

use poise::serenity_prelude::{Context, EditMessage, UserId};

use crate::PendingQuestionGroup;
use crate::Data;
use crate::claude_settings::{self, ClaudeSettingsError, MergeOutcome};
use crate::claude_settings::rule::{Scope, build_rule_text, scope_to_path};
use crate::db::repository;
use crate::error::PidoryError;
use crate::handler::discord_notifier::DiscordNotifier;
use crate::handler::message::interaction_kind::{CancelStage, InteractionKind, PermissionAction};
use crate::handler::permission_ui::{
    DisableReason, PermAction, build_permission_message_parts, disable_permission_buttons,
    dismiss_pending_by_tool, parse_permission_custom_id,
};
use crate::handler::{cleanup::cleanup_session_state, question_ui, reset_ui};
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

/// Cancels a question (single or multi-question group) by sending Deny to Claude
/// and disabling all related Discord messages.
pub(super) async fn cancel_question(
    data: &Data,
    ctx: &Context,
    request_id: &str,
    channel_id: poise::serenity_prelude::ChannelId,
    lang: Lang,
) {
    let canceled_label = lang.question_canceled_label();

    if let Some((group_id, _)) = question_ui::parse_sub_request_id(request_id) {
        // Multi-question: remove group → send Deny via real response_tx
        let group = data.pending_question_groups.lock().await.remove(&group_id);
        if let Some(g) = group {
            let _ = g.response_tx.send(PermissionDecision::Deny);
            let total = question_ui::question_count(&g.input);
            let mut perms = data.pending_permissions.lock().await;
            let mut to_disable = Vec::new();
            for idx in 0..total {
                let sub_id = question_ui::make_sub_request_id(&group_id, idx);
                // p.response_tx here is a dummy_tx; dropping it via scope-end is harmless.
                if let Some(p) = perms.remove(&sub_id) {
                    to_disable.push(p.message_id);
                }
            }
            drop(perms);
            for mid in to_disable {
                let _ = question_ui::disable_question_components_with_label(
                    ctx,
                    channel_id,
                    mid,
                    canceled_label,
                )
                .await;
            }
        }
    } else {
        // Single question: pending_permissions has the real response_tx
        let pending = data.pending_permissions.lock().await.remove(request_id);
        if let Some(p) = pending {
            let _ = p.response_tx.send(PermissionDecision::Deny);
            let _ = question_ui::disable_question_components_with_label(
                ctx,
                channel_id,
                p.message_id,
                canceled_label,
            )
            .await;
        }
    }
}

/// Handles a question answer — either direct (single question) or group (multi-question).
///
/// For single questions, the caller has already removed the PendingPermission and passes
/// its `response_tx` directly. For multi-question groups, the answer is stored in the
/// PendingQuestionGroup; when all answers are collected, the group's `response_tx` fires.
///
/// `question_text` is used as the key in the `answers` map — Claude CLI's
/// AskUserQuestion tool (≥ 2.1.121) looks up answers by the exact `question.question`
/// string when rendering the tool result, so a mismatched key (`q_0`/`q_1`) yielded
/// `"User has answered your questions:"` with no answers visible to the agent.
///
/// Completion is tracked via `group.answered` (a `HashSet<usize>` keyed by sub-question
/// index), not `group.answers.len()`. The `answers` map is keyed by question text and
/// would silently collide if Claude sent two questions with identical text — or if
/// `resolve_question_text` fell back to `""` for malformed input — leaving the group
/// stuck even after the user answered every question. See PR #275 follow-up.
pub(super) async fn handle_question_answer(
    data: &Data,
    request_id: &str,
    answer: String,
    question_text: String,
    response_tx: tokio::sync::oneshot::Sender<PermissionDecision>,
) {
    if let Some((group_id, q_idx)) = question_ui::parse_sub_request_id(request_id) {
        // Multi-question group member — store answer in group
        // The caller's response_tx is a dummy; drop it and use the group's instead.
        drop(response_tx);
        let mut groups = data.pending_question_groups.lock().await;
        if let Some(group) = groups.get_mut(&group_id) {
            let complete = record_group_answer(group, q_idx, question_text, answer);
            if complete {
                let group = groups.remove(&group_id).unwrap();
                let _ = group
                    .response_tx
                    .send(PermissionDecision::Answer(group.answers));
            }
        } else {
            tracing::warn!("PendingQuestionGroup not found for group_id={}", group_id);
        }
    } else {
        // Single question — send directly via the caller's response_tx
        let answers = HashMap::from([(question_text, answer)]);
        let _ = response_tx.send(PermissionDecision::Answer(answers));
    }
}

/// Records a single sub-question answer in a multi-question group. Returns
/// `true` when the group is complete (all sub-question indices answered).
///
/// Completion uses the index-keyed `answered` set, not `answers.len()`, because
/// the `answers` map is keyed by question text — duplicate texts (or empty
/// fallback keys from `resolve_question_text`) silently collapse, which would
/// leave the group permanently un-complete if `len()` were the gate.
pub(super) fn record_group_answer(
    group: &mut PendingQuestionGroup,
    q_idx: usize,
    question_text: String,
    answer: String,
) -> bool {
    if !question_text.is_empty() && group.answers.contains_key(&question_text) {
        tracing::warn!(
            "AskUserQuestion duplicate question text overwriting prior answer (q_idx={}, key={:?})",
            q_idx,
            question_text
        );
    }
    group.answers.insert(question_text, answer);
    group.answered.insert(q_idx);
    group.answered.len() >= group.total
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

    let kind = InteractionKind::from_custom_id(&component.data.custom_id);

    match kind {
        Some(InteractionKind::Permission { request_id: _, action }) => {
            handle_permission(ctx, component, data, action).await
        }
        Some(InteractionKind::QuestionOption { request_id, index }) => {
            handle_question_option(ctx, component, data, request_id, index).await
        }
        Some(InteractionKind::QuestionText { request_id }) => {
            handle_question_text(ctx, component, data, request_id).await
        }
        Some(InteractionKind::QuestionSelect { request_id }) => {
            handle_question_select(ctx, component, data, request_id).await
        }
        Some(InteractionKind::QuestionCancel { request_id, stage }) => {
            handle_question_cancel(ctx, component, data, request_id, stage).await
        }
        Some(InteractionKind::Reset { thread_id, action }) => {
            handle_reset(ctx, component, data, thread_id, action).await
        }
        Some(InteractionKind::NextStep { thread_id, skill }) => {
            handle_next_step(ctx, component, data, thread_id, skill).await
        }
        None => Ok(()),
    }
}

/// Returns the default scope for AlwaysAllow operations.
/// P1.3 (#288) 에서 DB user_settings 테이블 조회로 교체
fn default_scope() -> Scope {
    Scope::Project
}

async fn handle_permission(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    _legacy_action: PermissionAction,
) -> Result<(), PidoryError> {
    let lang = data.config.language;
    let channel_id = component.channel_id;

    // Re-parse with the full PermAction parser (handles all 6 variants including
    // always:exact, always:prefix, scope:toggle, etc.).
    let Some((request_id, perm_action)) =
        parse_permission_custom_id(&component.data.custom_id)
    else {
        return Ok(());
    };

    // Auth check — must happen before defer to send ephemeral on failure.
    let Some(_triggered_by) =
        verify_component_auth(component, ctx, data, &request_id, lang).await
    else {
        return Ok(());
    };

    // Defer immediately (3 s Discord timeout). Followups have 15 min window.
    component
        .create_response(
            ctx,
            poise::serenity_prelude::CreateInteractionResponse::Acknowledge,
        )
        .await
        .ok();

    match perm_action {
        PermAction::Once => {
            let pending = data.pending_permissions.lock().await.remove(&request_id);
            if let Some(p) = pending {
                let tool_name = p.tool_name.clone();
                let message_id = p.message_id;
                tracing::info!(request_id = %request_id, action = "once", "permission button clicked");
                let _ = p.response_tx.send(PermissionDecision::Allow);
                let _ = disable_permission_buttons(
                    ctx,
                    channel_id,
                    message_id,
                    DisableReason::Once,
                    &tool_name,
                    lang,
                )
                .await;
            }
        }

        PermAction::Deny => {
            let pending = data.pending_permissions.lock().await.remove(&request_id);
            if let Some(p) = pending {
                let tool_name = p.tool_name.clone();
                let message_id = p.message_id;
                tracing::info!(request_id = %request_id, action = "deny", "permission button clicked");
                let _ = p.response_tx.send(PermissionDecision::Deny);
                let _ = disable_permission_buttons(
                    ctx,
                    channel_id,
                    message_id,
                    DisableReason::Deny,
                    &tool_name,
                    lang,
                )
                .await;
            }
        }

        PermAction::ScopeToggle => {
            // Mutate scope_override in-place — do NOT remove from map.
            let update = {
                let mut map = data.pending_permissions.lock().await;
                let Some(entry) = map.get_mut(&request_id) else {
                    return Ok(());
                };
                let current = entry
                    .scope_override
                    .clone()
                    .unwrap_or_else(default_scope);
                let new_scope = current.flip();
                entry.scope_override = Some(new_scope.clone());
                let tool = entry.tool_name.clone();
                let input = entry.input.clone().unwrap_or(serde_json::json!({}));
                let triggered_by = entry.triggered_by;
                let message_id = entry.message_id;
                (new_scope, tool, input, triggered_by, message_id)
            };
            let (scope, tool, input, triggered_by, message_id) = update;
            tracing::info!(request_id = %request_id, ?scope, "permission scope toggled");
            let (content, components) = build_permission_message_parts(
                &tool,
                &input,
                &request_id,
                None,
                triggered_by,
                scope,
                lang,
            );
            let edit = EditMessage::new().content(content).components(components);
            let _ = channel_id.edit_message(ctx, message_id, edit).await;
        }

        PermAction::AllowAlways(rule_kind) => {
            let pending = data.pending_permissions.lock().await.remove(&request_id);
            let Some(pending) = pending else {
                return Ok(());
            };

            let scope = pending.scope_override.clone().unwrap_or_else(default_scope);
            let tool_name = pending.tool_name.clone();
            let input = pending.input.clone().unwrap_or(serde_json::json!({}));
            let thread_id = pending.thread_id.clone();
            let message_id = pending.message_id;

            tracing::info!(
                request_id = %request_id,
                tool_name = %tool_name,
                ?rule_kind,
                ?scope,
                "permission AlwaysAllow clicked"
            );

            // Build rule text
            let rule_text = match build_rule_text(&tool_name, &input, rule_kind.clone()) {
                Some(r) => r,
                None => {
                    tracing::warn!(
                        request_id = %request_id,
                        tool_name = %tool_name,
                        "rule_kind mismatch — no rule text produced"
                    );
                    let _ = disable_permission_buttons(
                        ctx,
                        channel_id,
                        message_id,
                        DisableReason::AllowAlwaysFailed {
                            reason: "rule_kind mismatch".into(),
                        },
                        &tool_name,
                        lang,
                    )
                    .await;
                    return Ok(());
                }
            };

            // Resolve settings file path from scope + project root
            let channel_id_str = channel_id.to_string();
            let project = match repository::get_project_by_channel(&data.db, &channel_id_str).await
            {
                Ok(Some(p)) => p,
                _ => {
                    let _ = disable_permission_buttons(
                        ctx,
                        channel_id,
                        message_id,
                        DisableReason::AllowAlwaysFailed {
                            reason: "project not registered".into(),
                        },
                        &tool_name,
                        lang,
                    )
                    .await;
                    return Ok(());
                }
            };
            let project_root = std::path::PathBuf::from(&project.path);
            let home = std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
            let settings_path = scope_to_path(scope.clone(), &project_root, &home);

            // Call atomic editor with Discord conflict notifier
            let notifier = DiscordNotifier {
                ctx: ctx.clone(),
                interaction: component.clone(),
                lang,
            };
            let result = claude_settings::add_permission(&settings_path, &rule_text, &notifier).await;

            let disable_reason = match result {
                Ok(MergeOutcome::Added) => DisableReason::AllowAlwaysSuccess {
                    rule_text: rule_text.clone(),
                },
                Ok(MergeOutcome::AlreadyPresent) => DisableReason::AllowAlwaysAlreadyPresent,
                Ok(MergeOutcome::ConflictResolved) => DisableReason::AllowAlwaysConflictResolved,
                Err(ref e)
                    if matches!(
                        e,
                        ClaudeSettingsError::LockConflict { .. }
                            | ClaudeSettingsError::LockTimeout { .. }
                    ) =>
                {
                    DisableReason::AllowAlwaysLockTimeout
                }
                Err(e) => DisableReason::AllowAlwaysFailed {
                    reason: format!("{}", e),
                },
            };

            // Send decision to worker only on success; on failure leave worker to timeout.
            let success = matches!(
                disable_reason,
                DisableReason::AllowAlwaysSuccess { .. }
                    | DisableReason::AllowAlwaysAlreadyPresent
                    | DisableReason::AllowAlwaysConflictResolved
            );
            if success {
                let _ = pending.response_tx.send(PermissionDecision::AllowAlways {
                    rule_kind: rule_kind.clone(),
                    scope: scope.clone(),
                });
                // Dismiss other pending requests for the same tool in this thread
                let dismissed = dismiss_pending_by_tool(
                    &data.pending_permissions,
                    &thread_id,
                    &tool_name,
                    PermissionDecision::AllowAlways {
                        rule_kind: rule_kind.clone(),
                        scope: scope.clone(),
                    },
                    &request_id,
                )
                .await;
                for d in &dismissed {
                    let _ = disable_permission_buttons(
                        ctx,
                        channel_id,
                        d.message_id,
                        DisableReason::AllowAlwaysAlreadyPresent,
                        &tool_name,
                        lang,
                    )
                    .await;
                    tracing::info!(
                        thread_id = %d.thread_id,
                        request_id = %d.request_id,
                        tool_name = %tool_name,
                        "permission auto-dismissed by always_allow chain"
                    );
                }
            }
            // Note: on failure, pending.response_tx is dropped here.
            // Worker will timeout and handle accordingly.

            let _ = disable_permission_buttons(
                ctx,
                channel_id,
                message_id,
                disable_reason,
                &tool_name,
                lang,
            )
            .await;
        }
    }

    Ok(())
}

async fn handle_question_option(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    request_id: String,
    option_index: usize,
) -> Result<(), PidoryError> {
    let lang = data.config.language;

    let Some(_triggered_by) = verify_component_auth(component, ctx, data, &request_id, lang).await
    else {
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
        let label = question_ui::resolve_option_label(&input, question_index, option_index);
        let question_text = question_ui::resolve_question_text(&input, question_index);
        handle_question_answer(
            data,
            &request_id,
            label.clone(),
            question_text,
            p.response_tx,
        )
        .await;
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

    Ok(())
}

async fn handle_question_text(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    request_id: String,
) -> Result<(), PidoryError> {
    let lang = data.config.language;

    let Some(_triggered_by) = verify_component_auth(component, ctx, data, &request_id, lang).await
    else {
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

    Ok(())
}

async fn handle_question_select(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    request_id: String,
) -> Result<(), PidoryError> {
    let lang = data.config.language;

    let Some(_triggered_by) = verify_component_auth(component, ctx, data, &request_id, lang).await
    else {
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
        let label = question_ui::resolve_option_label(&input, question_index, selected_index);
        let question_text = question_ui::resolve_question_text(&input, question_index);
        handle_question_answer(
            data,
            &request_id,
            label.clone(),
            question_text,
            p.response_tx,
        )
        .await;
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

    Ok(())
}

async fn handle_question_cancel(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    request_id: String,
    stage: CancelStage,
) -> Result<(), PidoryError> {
    let lang = data.config.language;

    match stage {
        CancelStage::Ask => {
            let Some(_triggered_by) =
                verify_component_auth(component, ctx, data, &request_id, lang).await
            else {
                return Ok(());
            };

            component
                .create_response(
                    ctx,
                    poise::serenity_prelude::CreateInteractionResponse::Message(
                        question_ui::create_cancel_confirm_message(&request_id, lang),
                    ),
                )
                .await
                .ok();
        }
        CancelStage::Confirm => {
            let Some(_triggered_by) =
                verify_component_auth(component, ctx, data, &request_id, lang).await
            else {
                return Ok(());
            };

            // Update ephemeral message to show cancellation
            component
                .create_response(
                    ctx,
                    poise::serenity_prelude::CreateInteractionResponse::UpdateMessage(
                        poise::serenity_prelude::CreateInteractionResponseMessage::new()
                            .content(format!("-# {}", lang.question_canceled_label()))
                            .components(vec![]),
                    ),
                )
                .await
                .ok();

            cancel_question(data, ctx, &request_id, component.channel_id, lang).await;
        }
        CancelStage::Abort => {
            // The ephemeral confirm message is only visible to the clicker, so no auth check is
            // strictly necessary. We still parse the id for correctness but skip verify_component_auth.
            // Collapse the ephemeral confirm dialog back to a neutral indicator.
            component
                .create_response(
                    ctx,
                    poise::serenity_prelude::CreateInteractionResponse::UpdateMessage(
                        poise::serenity_prelude::CreateInteractionResponseMessage::new()
                            .content("-# ↩️")
                            .components(vec![]),
                    ),
                )
                .await
                .ok();
        }
    }

    Ok(())
}

async fn handle_reset_confirm(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    thread_id: &str,
    channel_id: poise::serenity_prelude::ChannelId,
    lang: crate::i18n::Lang,
) -> Result<(), PidoryError> {
    let pending = data.pending_resets.lock().await.remove(thread_id);
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
            component.message.id,
            reset_ui::ResetOutcome::Expired,
        )
        .await
        .ok();
        return Ok(());
    };
    let _ = data.sessions.interrupt_session(thread_id).await;
    match data.sessions.kill_session(thread_id).await {
        Ok(()) | Err(PidoryError::NotFound(_)) => {}
        Err(e) => {
            channel_id
                .say(ctx, format!("❌ {}", lang.error_with(&e)))
                .await
                .ok();
            return Ok(());
        }
    }
    cleanup_session_state(data, thread_id, ctx).await;
    let _ = repository::delete_session(&data.db, thread_id).await;
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
    Ok(())
}

async fn handle_reset_cancel(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    thread_id: &str,
    channel_id: poise::serenity_prelude::ChannelId,
    lang: crate::i18n::Lang,
) -> Result<(), PidoryError> {
    let pending = data.pending_resets.lock().await.remove(thread_id);
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
    Ok(())
}

async fn handle_reset(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    thread_id: String,
    reset_action: reset_ui::ResetAction,
) -> Result<(), PidoryError> {
    let lang = data.config.language;
    let channel_id = component.channel_id;
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
    component
        .create_response(
            ctx,
            poise::serenity_prelude::CreateInteractionResponse::Acknowledge,
        )
        .await
        .ok();
    match reset_action {
        reset_ui::ResetAction::Confirm => {
            handle_reset_confirm(ctx, component, data, &thread_id, channel_id, lang).await?;
        }
        reset_ui::ResetAction::Cancel => {
            handle_reset_cancel(ctx, component, data, &thread_id, channel_id, lang).await?;
        }
    }
    Ok(())
}

async fn handle_next_step(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    thread_id: String,
    skill_name: String,
) -> Result<(), PidoryError> {
    let lang = data.config.language;

    component
        .create_response(
            ctx,
            poise::serenity_prelude::CreateInteractionResponse::UpdateMessage(
                poise::serenity_prelude::CreateInteractionResponseMessage::new().components(vec![]),
            ),
        )
        .await
        .ok();

    let is_owner = component.user.id == UserId::new(data.config.discord.owner_id);
    if !is_owner {
        component
            .create_followup(
                ctx,
                poise::serenity_prelude::CreateInteractionResponseFollowup::new()
                    .content(format!("❌ {}", lang.no_permission()))
                    .ephemeral(true),
            )
            .await
            .ok();
        return Ok(());
    }

    if data
        .session_states
        .lock()
        .await
        .get_mut(&thread_id)
        .and_then(|s| s.next_step_button.take())
        .is_none()
    {
        return Ok(());
    }

    let channel_id = component.channel_id;
    let msg_id = component.message.id;

    if !data.sessions.session_exists(&thread_id).await {
        component
            .create_followup(
                ctx,
                poise::serenity_prelude::CreateInteractionResponseFollowup::new()
                    .content(format!("❌ {}", lang.no_session_in_thread()))
                    .ephemeral(true),
            )
            .await
            .ok();
        return Ok(());
    }

    let cli_command = super::helpers::format_cli_command("skill", Some(&skill_name));
    super::execute_in_session(
        ctx,
        data,
        &thread_id,
        channel_id,
        msg_id,
        &cli_command,
        component.user.id,
    )
    .await
    .ok();

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
        let input = p.input.unwrap_or_default();
        let question_text = question_ui::resolve_question_text(&input, question_index);
        handle_question_answer(
            data,
            &request_id,
            answer.clone(),
            question_text,
            p.response_tx,
        )
        .await;
        question_ui::disable_question_components(ctx, modal.channel_id, message_id, &answer, lang)
            .await
            .ok();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use poise::serenity_prelude::UserId;
    use std::collections::{HashMap, HashSet};
    use tokio::sync::oneshot;

    fn make_group(total: usize) -> PendingQuestionGroup {
        let (tx, _rx) = oneshot::channel::<PermissionDecision>();
        PendingQuestionGroup {
            response_tx: tx,
            input: serde_json::json!({}),
            answers: HashMap::new(),
            answered: HashSet::new(),
            total,
            thread_id: "thread".to_string(),
            triggered_by: UserId::new(1),
        }
    }

    #[test]
    fn record_group_answer_completes_with_unique_texts() {
        let mut g = make_group(2);
        assert!(!record_group_answer(&mut g, 0, "Q0?".into(), "A".into()));
        assert!(record_group_answer(&mut g, 1, "Q1?".into(), "B".into()));
        assert_eq!(g.answers.len(), 2);
        assert_eq!(g.answered.len(), 2);
    }

    /// Regression: prior to this fix, completion used `answers.len()`, so two
    /// questions with identical text (LLM happens to phrase them the same)
    /// would collide on insert and the group would never complete — the
    /// Discord turn would hang until the user clicked Cancel or killed the
    /// session. With `answered` (index-keyed) as the gate, completion fires
    /// correctly even though the answers map collapsed to a single entry.
    #[test]
    fn record_group_answer_completes_even_with_duplicate_texts() {
        let mut g = make_group(2);
        assert!(!record_group_answer(&mut g, 0, "Same?".into(), "A".into()));
        assert!(record_group_answer(&mut g, 1, "Same?".into(), "B".into()));
        assert_eq!(g.answers.len(), 1, "duplicate text collapses to one entry (last write wins)");
        assert_eq!(g.answered.len(), 2, "completion counter still accurate");
    }

    /// Regression: if `resolve_question_text` falls back to `""` for malformed
    /// input (out-of-bounds index, missing `question` field), every sub-question
    /// would share the empty key and collide. Same hang as the duplicate-text
    /// case. Verify the `answered` set still tracks completion correctly.
    #[test]
    fn record_group_answer_completes_even_with_empty_fallback_keys() {
        let mut g = make_group(3);
        assert!(!record_group_answer(&mut g, 0, "".into(), "A".into()));
        assert!(!record_group_answer(&mut g, 1, "".into(), "B".into()));
        assert!(record_group_answer(&mut g, 2, "".into(), "C".into()));
        assert_eq!(g.answers.len(), 1);
        assert_eq!(g.answered.len(), 3);
    }

    #[test]
    fn record_group_answer_idempotent_on_same_index() {
        // Defensive: Discord disables the buttons after a click, but if two
        // events for the same index somehow arrive, the index set should not
        // double-count.
        let mut g = make_group(2);
        assert!(!record_group_answer(&mut g, 0, "Q?".into(), "A1".into()));
        assert!(!record_group_answer(&mut g, 0, "Q?".into(), "A2".into()));
        assert_eq!(g.answered.len(), 1);
    }
}
