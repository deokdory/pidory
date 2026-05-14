use std::collections::HashMap;

use poise::serenity_prelude::{
    Context, CreateInteractionResponse, CreateInteractionResponseMessage, UserId,
};

use crate::PendingQuestionGroup;
use crate::Data;
use crate::claude_settings::rule::{RuleKind, Scope, build_rule_texts, default_scope, scope_to_path};
use crate::claude_settings::{self, ClaudeSettingsError, MergeOutcome};
use crate::db::repository;
use crate::error::PidoryError;
use crate::handler::message::interaction_kind::{CancelStage, InteractionKind};
use crate::handler::permission_ui::{
    DisableReason, PermAction, build_level2_message_parts, build_processing_message_parts,
    disable_permission_buttons, dismiss_pending_by_tool,
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
        Some(InteractionKind::Permission { request_id, action }) => {
            handle_permission(ctx, component, data, request_id, action).await
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

async fn handle_permission(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    request_id: String,
    perm_action: PermAction,
) -> Result<(), PidoryError> {
    let lang = data.config.language;
    let channel_id = component.channel_id;

    // Auth check — must happen before defer to send ephemeral on failure.
    let Some(_triggered_by) =
        verify_component_auth(component, ctx, data, &request_id, lang).await
    else {
        // pending 없는 stale UI 케이스: verify_component_auth 가 None 반환하지만 ACK 미송신.
        // unauthorized 케이스는 이미 ephemeral 응답을 보냈으므로 두 번째 Acknowledge 는
        // Discord 가 HTTP 400 으로 reject — .ok() 로 silently ignore.
        component
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await
            .ok();
        return Ok(());
    };

    // ScopeToggle 은 별도 분기로 처리. UpdateMessage 로 한 번에 메시지 갱신
    // (Acknowledge + edit_message 2-call 보다 안전하고 race-free, review #297 w3).
    if matches!(perm_action, PermAction::ScopeToggle) {
        return handle_scope_toggle(ctx, component, data, &request_id, lang).await;
    }

    // ExpandAlways: Level 1 → Level 2 UI expand. UpdateMessage 단일 호출, 영속화 X.
    if matches!(perm_action, PermAction::ExpandAlways) {
        return handle_allow_always_expand(ctx, component, data, &request_id, lang).await;
    }

    // AllowAlways: 진입 즉시 UpdateMessage (Discord 3초 ACK 보장) + N=3 retry.
    // Acknowledge 없이 UpdateMessage 로 직접 처리 — ScopeToggle/ExpandAlways 패턴 동일.
    if let PermAction::AllowAlways(rule_kind) = perm_action {
        return handle_allow_always(ctx, component, data, &request_id, rule_kind, lang).await;
    }

    // serenity::Acknowledge = Discord type 6 (DEFERRED_UPDATE_MESSAGE) — component
    // interaction 의 정석. 사용자에게 loading state 안 보이고 이후 edit_message
    // 로 메시지 갱신.
    component
        .create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await
        .ok();

    match perm_action {
        PermAction::Once => {
            let pending = data.pending_permissions.lock().await.remove(&request_id);
            if let Some(p) = pending {
                let tool_name = p.tool_name.clone();
                let tool_input = p.input.clone().unwrap_or(serde_json::json!({}));
                let message_id = p.message_id;
                tracing::info!(request_id = %request_id, action = "once", "permission button clicked");
                let _ = p.response_tx.send(PermissionDecision::Allow);
                let _ = disable_permission_buttons(
                    ctx,
                    channel_id,
                    message_id,
                    DisableReason::Once,
                    &tool_name,
                    &tool_input,
                    lang,
                )
                .await;
            }
        }

        PermAction::Deny => {
            let pending = data.pending_permissions.lock().await.remove(&request_id);
            if let Some(p) = pending {
                let tool_name = p.tool_name.clone();
                let tool_input = p.input.clone().unwrap_or(serde_json::json!({}));
                let message_id = p.message_id;
                tracing::info!(request_id = %request_id, action = "deny", "permission button clicked");
                let _ = p.response_tx.send(PermissionDecision::Deny);
                let _ = disable_permission_buttons(
                    ctx,
                    channel_id,
                    message_id,
                    DisableReason::Deny,
                    &tool_name,
                    &tool_input,
                    lang,
                )
                .await;
            }
        }

        PermAction::ScopeToggle => unreachable!("handled above"),
        PermAction::ExpandAlways => unreachable!("handled above"),
        PermAction::AllowAlways(_) => unreachable!("handled above"),
    }

    Ok(())
}

/// ScopeToggle: Project ↔ Global 전환. UpdateMessage 로 메시지 즉시 갱신
/// (Acknowledge + edit_message 의 race / 2-call 회피, review #297 w3).
async fn handle_scope_toggle(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    request_id: &str,
    lang: Lang,
) -> Result<(), PidoryError> {
    let update = {
        let mut map = data.pending_permissions.lock().await;
        let Some(entry) = map.get_mut(request_id) else {
            // pending 이 없으면 ACK 만 (메시지 변경 안 함)
            component
                .create_response(ctx, CreateInteractionResponse::Acknowledge)
                .await
                .ok();
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
        let decision_reason = entry.decision_reason.clone();
        let cwd = entry.cwd.clone();
        let additional_dirs_arc = entry.additional_dirs.clone();
        let file_path_opt = entry.file_path.clone();
        (new_scope, tool, input, triggered_by, decision_reason, cwd, additional_dirs_arc, file_path_opt)
    };
    let (scope, tool, input, triggered_by, decision_reason, cwd, additional_dirs_arc, file_path_opt) = update;
    let file_path_str = file_path_opt.as_deref();
    tracing::info!(request_id = %request_id, ?scope, "permission scope toggled");

    let (content, components) = build_level2_message_parts(
        &tool,
        &input,
        request_id,
        decision_reason.as_deref(),
        triggered_by,
        scope,
        lang,
        file_path_str,
        &cwd,
        &additional_dirs_arc,
    );
    let response = CreateInteractionResponse::UpdateMessage(
        CreateInteractionResponseMessage::new()
            .content(content)
            .components(components),
    );
    if let Err(e) = component.create_response(ctx, response).await {
        tracing::warn!(request_id = %request_id, error = %e, "scope toggle UpdateMessage 실패");
    }
    Ok(())
}

/// ExpandAlways: Level 1 "항상 허용" 클릭 → Level 2 UI 로 메시지 즉시 갱신.
///
/// - response_tx 송신 X (영속화 결정 아님, UI expand 만)
/// - pending entry remove X (영속화 결정 시점에 remove)
/// - scope_override 변경 X (expand 는 scope 변경 아님)
/// - UpdateMessage 단일 호출 → race-free ACK 동시 처리 (review #297 w3)
async fn handle_allow_always_expand(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    request_id: &str,
    lang: Lang,
) -> Result<(), PidoryError> {
    let update = {
        let map = data.pending_permissions.lock().await;
        let Some(entry) = map.get(request_id) else {
            // pending 이 없으면 ACK 만 (메시지 변경 안 함)
            component
                .create_response(ctx, CreateInteractionResponse::Acknowledge)
                .await
                .ok();
            return Ok(());
        };
        let scope = entry.scope_override.clone().unwrap_or_else(default_scope);
        let tool = entry.tool_name.clone();
        let input = entry.input.clone().unwrap_or(serde_json::json!({}));
        let triggered_by = entry.triggered_by;
        let decision_reason = entry.decision_reason.clone();
        let cwd = entry.cwd.clone();
        let additional_dirs_arc = entry.additional_dirs.clone();
        let file_path_opt = entry.file_path.clone();
        (scope, tool, input, triggered_by, decision_reason, cwd, additional_dirs_arc, file_path_opt)
    };
    let (scope, tool, input, triggered_by, decision_reason, cwd, additional_dirs_arc, file_path_opt) = update;
    let file_path_str = file_path_opt.as_deref();
    tracing::info!(request_id = %request_id, ?scope, "permission ExpandAlways clicked");

    let (content, components) = build_level2_message_parts(
        &tool,
        &input,
        request_id,
        decision_reason.as_deref(),
        triggered_by,
        scope,
        lang,
        file_path_str,
        &cwd,
        &additional_dirs_arc,
    );
    let response = CreateInteractionResponse::UpdateMessage(
        CreateInteractionResponseMessage::new()
            .content(content)
            .components(components),
    );
    if let Err(e) = component.create_response(ctx, response).await {
        tracing::warn!(request_id = %request_id, error = %e, "always:expand UpdateMessage 실패");
    }
    Ok(())
}

/// AllowAlways: rule 을 settings 에 저장하고 worker 에 결정 송신.
///
/// - 진입 즉시 UpdateMessage (처리 중 메시지 + 모든 버튼 disabled) — Discord 3초 ACK 보장.
/// - N=3 retry 루프: LockConflict/LockTimeout 시 100ms sleep 후 재시도.
/// - attempt 2/3 부터 EditMessage 로 재시도 카운터 표시.
/// - 성공: AllowAlways 결정 송신 + AllowAlwaysSuccess 메시지.
/// - 3회 모두 LockConflict → Deny 송신 + AllowAlwaysMaxRetries 메시지.
/// - 기타 에러: Deny 송신 + AllowAlwaysFailed 메시지 (review #297 c2).
/// - dismiss 정책: RuleKind::Tool 일 때만 같은 tool 의 다른 pending 자동 dismiss
///   (Exact/Prefix/Domain 은 본 request 만 처리 — 권한 누출 방지, review #297 c1).
async fn handle_allow_always(
    ctx: &Context,
    component: &poise::serenity_prelude::ComponentInteraction,
    data: &Data,
    request_id: &str,
    rule_kind: RuleKind,
    lang: Lang,
) -> Result<(), PidoryError> {
    let channel_id = component.channel_id;

    // 0. pending 잠금 + remove
    let pending = data.pending_permissions.lock().await.remove(request_id);
    let Some(pending) = pending else {
        component
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await
            .ok();
        return Ok(());
    };

    let scope = pending.scope_override.clone().unwrap_or_else(default_scope);
    let tool_name = pending.tool_name.clone();
    let input = pending.input.clone().unwrap_or(serde_json::json!({}));
    let thread_id = pending.thread_id.clone();
    let message_id = pending.message_id;
    let triggered_by = pending.triggered_by;
    let cwd = pending.cwd.clone();
    let additional_dirs_arc = pending.additional_dirs.clone();
    let file_path_opt = pending.file_path.clone();

    tracing::info!(
        request_id = %request_id,
        tool_name = %tool_name,
        ?rule_kind,
        ?scope,
        "permission AllowAlways clicked"
    );

    // Helper: 실패 시 명시적 Deny 송신 + UI disable (review #297 c2)
    #[allow(clippy::too_many_arguments)]
    async fn fail_with(
        ctx: &Context,
        channel_id: poise::serenity_prelude::ChannelId,
        message_id: poise::serenity_prelude::MessageId,
        response_tx: tokio::sync::oneshot::Sender<PermissionDecision>,
        tool_name: &str,
        tool_input: &serde_json::Value,
        reason: DisableReason,
        lang: Lang,
        log_reason: &str,
    ) {
        tracing::warn!(tool_name = %tool_name, reason = log_reason, "AllowAlways failed; sending Deny");
        let _ = response_tx.send(PermissionDecision::Deny);
        let _ = disable_permission_buttons(ctx, channel_id, message_id, reason, tool_name, tool_input, lang)
            .await;
    }

    // 1. 진입 즉시 UpdateMessage — Discord 3초 ACK 보장
    let (proc_content, proc_components) = build_processing_message_parts(
        &tool_name,
        &input,
        request_id,
        triggered_by,
        scope.clone(),
        lang,
        None,
        file_path_opt.as_deref(),
        &cwd,
        &additional_dirs_arc,
    );
    let response = CreateInteractionResponse::UpdateMessage(
        CreateInteractionResponseMessage::new()
            .content(proc_content)
            .components(proc_components),
    );
    if let Err(e) = component.create_response(ctx, response).await {
        tracing::warn!(request_id = %request_id, error = %e, "AllowAlways UpdateMessage 실패");
    }

    // 2. rule 빌드 (복수형)
    let rules = build_rule_texts(&tool_name, &input, rule_kind.clone());
    if rules.is_empty() {
        fail_with(
            ctx,
            channel_id,
            message_id,
            pending.response_tx,
            &tool_name,
            &input,
            DisableReason::AllowAlwaysFailed {
                reason: "rule_kind mismatch".into(),
            },
            lang,
            "rule_kind mismatch",
        )
        .await;
        return Ok(());
    }
    let rule_strs: Vec<&str> = rules.iter().map(|s| s.as_str()).collect();

    // 3. Resolve project root.
    //
    // `component.channel_id` 는 스레드의 ID (parent channel 이 아님). `projects` 는 부모 channel_id PK.
    // → `pending.thread_id` 로 sessions 조회 → session.channel_id (부모) 로 projects 조회.
    //
    // `Err(_)` 와 `Ok(None)` 을 분리 처리 — DB 에러(연결 끊김 등)는 `tracing::error!` 로 기록해야
    // 운영 중 디버깅 가능 (review #298 w1).
    let session = match repository::get_session_by_thread(&data.db, &thread_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            fail_with(
                ctx,
                channel_id,
                message_id,
                pending.response_tx,
                &tool_name,
                &input,
                DisableReason::AllowAlwaysFailed {
                    reason: "session not found".into(),
                },
                lang,
                "session not found",
            )
            .await;
            return Ok(());
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                thread_id = %thread_id,
                "session lookup DB error"
            );
            fail_with(
                ctx,
                channel_id,
                message_id,
                pending.response_tx,
                &tool_name,
                &input,
                DisableReason::AllowAlwaysFailed {
                    reason: format!("DB error: {}", e),
                },
                lang,
                "session lookup DB error",
            )
            .await;
            return Ok(());
        }
    };
    let project = match repository::get_project_by_channel(&data.db, &session.channel_id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            fail_with(
                ctx,
                channel_id,
                message_id,
                pending.response_tx,
                &tool_name,
                &input,
                DisableReason::AllowAlwaysFailed {
                    reason: "project not registered".into(),
                },
                lang,
                "project not registered",
            )
            .await;
            return Ok(());
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                channel_id = %session.channel_id,
                "project lookup DB error"
            );
            fail_with(
                ctx,
                channel_id,
                message_id,
                pending.response_tx,
                &tool_name,
                &input,
                DisableReason::AllowAlwaysFailed {
                    reason: format!("DB error: {}", e),
                },
                lang,
                "project lookup DB error",
            )
            .await;
            return Ok(());
        }
    };
    let project_root = std::path::PathBuf::from(&project.path);
    let project_basename = project_root
        .file_name()
        .and_then(|os| os.to_str())
        .map(|s| s.to_string());

    // 4. Resolve HOME — Global scope 에서만 필요. 미설정 시 명시적 실패 (review #297 w1)
    let home = match std::env::var("HOME") {
        Ok(h) => std::path::PathBuf::from(h),
        Err(_) if matches!(scope, Scope::Global) => {
            fail_with(
                ctx,
                channel_id,
                message_id,
                pending.response_tx,
                &tool_name,
                &input,
                DisableReason::AllowAlwaysFailed {
                    reason: "HOME env not set; cannot resolve global settings".into(),
                },
                lang,
                "HOME not set for Global scope",
            )
            .await;
            return Ok(());
        }
        Err(_) => std::path::PathBuf::new(), // Project scope 에서는 사용 안 함
    };
    let settings_path = scope_to_path(scope.clone(), &project_root, &home);

    // 5. ConflictNotifier 인스턴스 생성
    // LoggingNotifier — silent notifier. retry 노이즈 회피 (review #298 v2 F-UI-6)
    // retry 진행 인디케이터 + 자동 거부 메시지로 사용자 안내 충분.
    let notifier = claude_settings::LoggingNotifier;

    // 6. N=3 retry 루프
    const MAX_RETRIES: u32 = 3;
    let mut last_err: Option<ClaudeSettingsError> = None;
    let mut outcome: Option<MergeOutcome> = None;

    for attempt in 1..=MAX_RETRIES {
        // attempt 2 부터 재시도 카운터 표시 (EditMessage)
        // attempt 1 은 진입 시 UpdateMessage 가 이미 처리 중 메시지를 표시함.
        if attempt >= 2 {
            let (retry_content, retry_components) = build_processing_message_parts(
                &tool_name,
                &input,
                request_id,
                triggered_by,
                scope.clone(),
                lang,
                Some((attempt, MAX_RETRIES)),
                file_path_opt.as_deref(),
                &cwd,
                &additional_dirs_arc,
            );
            let _ = channel_id
                .edit_message(
                    ctx,
                    message_id,
                    poise::serenity_prelude::EditMessage::new()
                        .content(retry_content)
                        .components(retry_components),
                )
                .await;
        }

        match claude_settings::add_permissions(&settings_path, &rule_strs, &notifier).await {
            Ok(o) => {
                outcome = Some(o);
                break;
            }
            Err(e @ ClaudeSettingsError::LockConflict { .. })
            | Err(e @ ClaudeSettingsError::LockTimeout { .. }) => {
                last_err = Some(e);
                if attempt < MAX_RETRIES {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
            Err(other) => {
                last_err = Some(other);
                break;
            }
        }
    }

    // 7. 결과 분기
    match outcome {
        Some(merge_outcome) => {
            // 성공 (Added / AlreadyPresent / ConflictResolved)
            let _ = pending.response_tx.send(PermissionDecision::AllowAlways {
                rule_kind: rule_kind.clone(),
                scope: scope.clone(),
            });

            // Claude CLI 가 settings.local.json 을 핫 리로드하지 않으므로
            // 다음 user message 도착 시 subprocess 를 --resume 으로 재시작 예약.
            // Added / AlreadyPresent / ConflictResolved 세 outcome 모두 동일 처리.
            data.pending_session_restart
                .lock()
                .await
                .insert(thread_id.clone());

            // c1 fix: RuleKind::Tool 일 때만 같은 tool 의 다른 pending 자동 dismiss.
            // Exact/Prefix/Domain 은 더 좁은 매칭이라 다른 명령까지 통과시키면 권한 누출.
            if matches!(rule_kind, RuleKind::Tool) {
                let dismissed = dismiss_pending_by_tool(
                    &data.pending_permissions,
                    &thread_id,
                    &tool_name,
                    PermissionDecision::AllowAlways {
                        rule_kind: rule_kind.clone(),
                        scope: scope.clone(),
                    },
                    request_id,
                )
                .await;
                // dismiss 된 메시지들도 disable (AutoDismissedByAlwaysChain)
                // triggering_rule: 첫 번째 rule (Tool 은 단일 rule)
                let triggering_rule = rules.first().cloned().unwrap_or_default();
                for d in &dismissed {
                    let _ = disable_permission_buttons(
                        ctx,
                        channel_id,
                        d.message_id,
                        DisableReason::AutoDismissedByAlwaysChain {
                            triggering_rule: triggering_rule.clone(),
                        },
                        &tool_name,
                        &d.input,
                        lang,
                    )
                    .await;
                    tracing::info!(
                        thread_id = %d.thread_id,
                        request_id = %d.request_id,
                        tool_name = %tool_name,
                        triggering_rule = %triggering_rule,
                        "permission auto-dismissed by AllowAlways(Tool) chain"
                    );
                }
            }

            // .gitignore 검사 — Project scope 영속 클릭 시에만 실행 (매번 안내, de-dup v1.0)
            if matches!(scope, Scope::Project) {
                check_gitignore_and_warn(ctx, channel_id, &project_root, lang).await;
            }

            // outcome 별 disable reason 분기 (w3 fix: Some(_) 와일드카드 → 명시적 분기)
            let disable_reason = match merge_outcome {
                MergeOutcome::Added => DisableReason::AllowAlwaysSuccess {
                    rules,
                    scope: scope.clone(),
                    project_basename,
                },
                MergeOutcome::AlreadyPresent => DisableReason::AllowAlwaysAlreadyPresent,
                MergeOutcome::ConflictResolved => DisableReason::AllowAlwaysConflictResolved,
            };

            let _ = disable_permission_buttons(
                ctx,
                channel_id,
                message_id,
                disable_reason,
                &tool_name,
                &input,
                lang,
            )
            .await;
        }
        None => {
            // 실패
            let _ = pending.response_tx.send(PermissionDecision::Deny);

            let disable_reason = match last_err {
                Some(ClaudeSettingsError::LockConflict { .. })
                | Some(ClaudeSettingsError::LockTimeout { .. }) => {
                    // N회 retry 모두 LockConflict/LockTimeout
                    tracing::warn!(
                        request_id = %request_id,
                        tool_name = %tool_name,
                        attempts = MAX_RETRIES,
                        "AllowAlways max retries exceeded; sending explicit Deny to worker"
                    );
                    DisableReason::AllowAlwaysMaxRetries {
                        attempts: MAX_RETRIES,
                    }
                }
                Some(other) => {
                    // 기타 에러 (c2 fix: 명시적 Deny)
                    tracing::warn!(
                        request_id = %request_id,
                        tool_name = %tool_name,
                        error = %other,
                        "AllowAlways atomic editor failed; sending explicit Deny to worker"
                    );
                    DisableReason::AllowAlwaysFailed {
                        reason: format!("{}", other),
                    }
                }
                None => unreachable!("loop must set either outcome or last_err"),
            };

            let _ = disable_permission_buttons(
                ctx,
                channel_id,
                message_id,
                disable_reason,
                &tool_name,
                &input,
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
            lang,
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
        lang,
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
        lang,
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

/// Project scope 영속 클릭 후 `.gitignore` 검사.
///
/// `<project_root>/.gitignore` 를 읽어 `.claude/settings.local.json` 또는
/// `.claude/*.local.json` 패턴이 없으면 thread 에 안내 메시지를 1회 전송한다.
/// 파일 없음 / 읽기 실패 → silent skip.
async fn check_gitignore_and_warn(
    ctx: &Context,
    channel_id: poise::serenity_prelude::ChannelId,
    project_root: &std::path::Path,
    lang: crate::i18n::Lang,
) {
    let gitignore_path = project_root.join(".gitignore");
    let content = match tokio::fs::read_to_string(&gitignore_path).await {
        Ok(c) => c,
        Err(_) => return, // 없거나 읽기 실패 → silent skip
    };

    let covered = content.lines().any(|line| {
        let trimmed = line.trim();
        // comment 줄 skip
        if trimmed.starts_with('#') {
            return false;
        }
        // .gitignore 에서 .claude/settings.local.json 을 커버하는 패턴 인식.
        // - 정확 경로 / root-anchored / recursive glob
        // - 디렉터리 단위 (.claude/, .claude)
        // - wildcard (*.local.json, settings.local.json)
        matches!(
            trimmed,
            ".claude/settings.local.json"
                | ".claude/*.local.json"
                | ".claude/"
                | ".claude"
                | "/.claude/settings.local.json"
                | "/.claude/*.local.json"
                | "/.claude/"
                | "/.claude"
                | "**/.claude/settings.local.json"
                | "**/.claude/*.local.json"
                | "**/.claude/"
                | "**/.claude"
                | ".claude/*"
                | "*.local.json"
                | "settings.local.json"
        )
    });

    if !covered {
        let advisory = lang.gitignore_missing_advisory();
        channel_id.say(ctx, advisory).await.ok();
    }
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
