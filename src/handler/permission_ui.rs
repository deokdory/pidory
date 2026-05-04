#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use poise::serenity_prelude::{
    ButtonStyle, ChannelId, Context, CreateActionRow, CreateButton, CreateMessage, EditMessage,
    MessageId, UserId,
};
use tokio::sync::{Mutex, mpsc};
use tracing::warn;

use crate::claude_settings::danger::classify_command;
use crate::claude_settings::rule::{
    RuleKind, Scope, available_rule_kinds, build_rule_text, default_scope,
};
use crate::error::PidoryError;
use crate::handler::formatter::inline_code;
use crate::handler::question_ui;
use crate::i18n::Lang;
use crate::subprocess::permission::{PermissionDecision, PermissionRequest};
use crate::{PendingPermission, PendingQuestionGroup};

/// Disabled "항상 허용" 레이블 버튼의 custom_id.
///
/// Discord 는 disabled 버튼 클릭 시 interaction 을 발사하지 않지만, 방어적으로
/// `parse_permission_custom_id` 가 이 ID 를 인식하지 않도록 강제한다 (test: `parse_perm_label_returns_none`).
pub const LABEL_BUTTON_CUSTOM_ID: &str = "perm:_label";

/// Permission 버튼 클릭 결과로 취할 행동.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermAction {
    /// 이번 한 번만 허용
    Once,
    /// 거부
    Deny,
    /// Scope(global/project) 토글
    ScopeToggle,
    /// 영구 허용 (rule 저장) — 저장 방식은 RuleKind로 결정
    AllowAlways(RuleKind),
}

/// `perm:{request_id}:{tail}` 형식의 custom_id를 파싱한다.
///
/// - `perm:` 접두사 없으면 `None`
/// - `request_id`에 `:` 포함 가능 — suffix strip 방식으로 tail 만 분리
/// - 알 수 없는 tail → `None`
///
/// Legacy 토큰 (`:allow`, `:always`) 도 backward-compat 으로 인식한다 (review #297 w4):
///   - `:allow`  → `PermAction::Once` (이전 Allow = 한 번만 허용)
///   - `:always` → `PermAction::AllowAlways(RuleKind::Tool)` (이전 Always = tool 전체 허용)
pub fn parse_permission_custom_id(custom_id: &str) -> Option<(String, PermAction)> {
    let rest = custom_id.strip_prefix("perm:")?;
    // tail suffix longest-first matching
    let (request_id, action) = if let Some(rid) = rest.strip_suffix(":scope:toggle") {
        (rid, PermAction::ScopeToggle)
    } else if let Some(rid) = rest.strip_suffix(":always:exact") {
        (rid, PermAction::AllowAlways(RuleKind::Exact))
    } else if let Some(rid) = rest.strip_suffix(":always:prefix") {
        (rid, PermAction::AllowAlways(RuleKind::Prefix))
    } else if let Some(rid) = rest.strip_suffix(":always:domain") {
        (rid, PermAction::AllowAlways(RuleKind::Domain))
    } else if let Some(rid) = rest.strip_suffix(":always:tool") {
        (rid, PermAction::AllowAlways(RuleKind::Tool))
    } else if let Some(rid) = rest.strip_suffix(":once") {
        (rid, PermAction::Once)
    } else if let Some(rid) = rest.strip_suffix(":deny") {
        (rid, PermAction::Deny)
    } else if let Some(rid) = rest.strip_suffix(":allow") {
        // Legacy: 이전 버전의 "Allow" 버튼 = 한 번만 허용
        (rid, PermAction::Once)
    } else if let Some(rid) = rest.strip_suffix(":always") {
        // Legacy: 이전 버전의 "Always Allow" 버튼 = tool 전체 허용
        (rid, PermAction::AllowAlways(RuleKind::Tool))
    } else {
        return None;
    };

    if request_id.is_empty() {
        return None;
    }

    Some((request_id.to_string(), action))
}

/// Internal helper: builds `(content, components)` for a permission message.
/// Used by both `create_permission_message` and `ScopeToggle` edit path.
pub fn build_permission_message_parts(
    tool_name: &str,
    input: &serde_json::Value,
    request_id: &str,
    decision_reason: Option<&str>,
    triggered_by: UserId,
    scope: Scope,
    lang: Lang,
) -> (String, Vec<CreateActionRow>) {
    let summary = format_tool_input_summary(tool_name, input);
    let reason = decision_reason
        .map(|r| format!("\n> {}", r))
        .unwrap_or_default();

    // 헤더 scope 표시
    let scope_label = match &scope {
        Scope::Project => "📁 project".to_string(),
        Scope::Global => "🌐 global  ⚠️ 모든 프로젝트에 적용됨".to_string(),
    };
    let header = format!(
        "🔒 <@{}>  {}  ·  scope: {}",
        triggered_by,
        inline_code(tool_name),
        scope_label,
    );

    // 미리보기: available_rule_kinds → build_rule_text 각 줄. 버튼 라벨과 1:1 대응.
    let kinds = available_rule_kinds(tool_name, input);
    let preview_lines: Vec<String> = kinds
        .iter()
        .filter_map(|kind| {
            build_rule_text(tool_name, input, kind.clone()).map(|rule_text| {
                // classify_command 호출 — skeleton (P1.5에서 활용), 현재 시각 변경 없음
                let _severity = classify_command(&rule_text);
                let prefix = match kind {
                    RuleKind::Exact => lang.btn_always_exact(),
                    RuleKind::Prefix => lang.btn_always_prefix(),
                    RuleKind::Domain => lang.btn_always_domain(),
                    RuleKind::Tool => lang.btn_always_tool(),
                };
                format!("{} → {}", prefix, inline_code(&rule_text))
            })
        })
        .collect();

    let preview_section = if preview_lines.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n{}\n{}",
            lang.msg_always_allow_options_header(),
            preview_lines.join("\n")
        )
    };

    let content = format!(
        "{}\n{}{}{}",
        header, summary, reason, preview_section
    );

    // Row 1: once + deny — 아이콘 없이 (Danger 색이 아이콘을 가려서 가독성 저하)
    let once_btn = CreateButton::new(format!("perm:{}:once", request_id))
        .label(lang.btn_once())
        .style(ButtonStyle::Success);
    let deny_btn = CreateButton::new(format!("perm:{}:deny", request_id))
        .label(lang.btn_deny())
        .style(ButtonStyle::Danger);
    let row1 = CreateActionRow::Buttons(vec![once_btn, deny_btn]);

    // Row 2: [항상 허용 disabled label] + always-allow buttons + scope toggle
    //
    // Discord 는 ActionRow 사이에 텍스트 삽입 불가 → disabled 버튼이 섹션 헤더 역할.
    // ActionRow 5버튼 한도 검증:
    //   Bash:             label + Exact + Prefix + Tool + scope = 5 ✓
    //   WebFetch(domain): label + Domain + Tool + scope         = 4 ✓
    //   Read/Edit/Write:  label + Exact + Tool + scope          = 4 ✓
    //   Grep/Glob/IP:     label + Tool + scope                  = 3 ✓
    let label_btn = CreateButton::new(LABEL_BUTTON_CUSTOM_ID)
        .label(lang.btn_always_allow())
        .style(ButtonStyle::Secondary)
        .disabled(true);

    let mut row2_buttons: Vec<CreateButton> = vec![label_btn];
    row2_buttons.extend(kinds.iter().map(|kind| match kind {
        RuleKind::Exact => CreateButton::new(format!("perm:{}:always:exact", request_id))
            .label(lang.btn_always_exact())
            .style(ButtonStyle::Primary),
        RuleKind::Prefix => CreateButton::new(format!("perm:{}:always:prefix", request_id))
            .label(lang.btn_always_prefix())
            .style(ButtonStyle::Secondary),
        RuleKind::Domain => CreateButton::new(format!("perm:{}:always:domain", request_id))
            .label(lang.btn_always_domain())
            .style(ButtonStyle::Secondary),
        // Tool 전체 허용은 매우 위험 — Danger style (review #297 s3)
        RuleKind::Tool => CreateButton::new(format!("perm:{}:always:tool", request_id))
            .label(lang.btn_always_tool())
            .style(ButtonStyle::Danger),
    }));

    let (scope_btn_label, scope_btn_style) = match &scope {
        Scope::Project => (lang.btn_scope_toggle_to_global(), ButtonStyle::Secondary),
        Scope::Global => (lang.btn_scope_toggle_to_project(), ButtonStyle::Primary),
    };
    row2_buttons.push(
        CreateButton::new(format!("perm:{}:scope:toggle", request_id))
            .label(scope_btn_label)
            .style(scope_btn_style),
    );

    // Discord ActionRow 5버튼 한도 — 향후 새 tool 의 RuleKind 가 늘면 컴파일 통과해도
    // Discord API 가 메시지 거부 (silent fail). debug build 에서 조기 검출 (review #298 s2).
    debug_assert!(
        row2_buttons.len() <= 5,
        "ActionRow exceeds Discord 5-button limit (tool={}, count={})",
        tool_name,
        row2_buttons.len(),
    );

    let row2 = CreateActionRow::Buttons(row2_buttons);

    (content, vec![row1, row2])
}

pub fn create_permission_message(
    tool_name: &str,
    input: &serde_json::Value,
    request_id: &str,
    decision_reason: Option<&str>,
    triggered_by: UserId,
    scope: Scope,
    lang: Lang,
) -> CreateMessage {
    let (content, components) = build_permission_message_parts(
        tool_name,
        input,
        request_id,
        decision_reason,
        triggered_by,
        scope,
        lang,
    );
    CreateMessage::new().content(content).components(components)
}

pub fn format_tool_input_summary(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "Bash" => {
            let command = input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("```\n{}\n```", command)
        }
        "Edit" | "Write" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("`{}`", file_path)
        }
        "Read" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("`{}`", file_path)
        }
        "Grep" => {
            let pattern = input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("`{}`", pattern)
        }
        "Glob" => {
            let pattern = input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("`{}`", pattern)
        }
        "WebFetch" => {
            let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
            format!("`{}`", url)
        }
        "WebSearch" => {
            let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("");
            format!("`{}`", query)
        }
        _ => input
            .as_object()
            .and_then(|obj| {
                obj.values().find_map(|v| v.as_str()).map(|s| {
                    let truncated: String = s.chars().take(100).collect();
                    format!("`{}`", truncated)
                })
            })
            .unwrap_or_default(),
    }
}

/// `disable_permission_buttons` 에 전달되는 결과 이유.
#[derive(Debug)]
pub enum DisableReason {
    /// 이번 한 번만 허용됨
    Once,
    /// 거부됨
    Deny,
    /// Always Allow 성공 — rule이 새로 추가됨 (MergeOutcome::Added)
    AllowAlwaysSuccess { rule_text: String },
    /// Already present — 동일 rule이 이미 존재함 (MergeOutcome::AlreadyPresent)
    AllowAlwaysAlreadyPresent,
    /// Conflict resolved — 충돌 규칙이 자동 해소됨 (MergeOutcome::ConflictResolved)
    AllowAlwaysConflictResolved,
    /// Lock timeout — 파일 잠금 대기 초과 (ClaudeSettingsError::LockTimeout 등)
    AllowAlwaysLockTimeout,
    /// 그 외 atomic editor 실패
    AllowAlwaysFailed { reason: String },
    /// 같은 tool 의 다른 pending 이 AlwaysAllow 처리되어 자동 취소됨 (review #297 s1)
    AutoDismissedByAlwaysChain { triggering_rule: String },
}

pub async fn disable_permission_buttons(
    ctx: &Context,
    channel_id: ChannelId,
    message_id: MessageId,
    reason: DisableReason,
    tool_name: &str,
    lang: Lang,
) -> Result<(), PidoryError> {
    let label = match reason {
        DisableReason::Once => match lang {
            Lang::Ko => "-# ✅ 한 번만 허용됨".to_string(),
            Lang::En => "-# ✅ Allowed once".to_string(),
        },
        DisableReason::Deny => match lang {
            Lang::Ko => "-# ❌ 거부됨".to_string(),
            Lang::En => "-# ❌ Denied".to_string(),
        },
        DisableReason::AllowAlwaysSuccess { rule_text } => {
            format!("-# {}", lang.msg_save_success(&rule_text))
        }
        DisableReason::AllowAlwaysAlreadyPresent => match lang {
            Lang::Ko => "-# 🔓 이미 등록됨".to_string(),
            Lang::En => "-# 🔓 Already present".to_string(),
        },
        DisableReason::AllowAlwaysConflictResolved => match lang {
            Lang::Ko => "-# 🔓 충돌 자동 해소됨".to_string(),
            Lang::En => "-# 🔓 Conflict resolved".to_string(),
        },
        DisableReason::AllowAlwaysLockTimeout => {
            format!("-# {}", lang.msg_save_failed_lock_timeout())
        }
        DisableReason::AllowAlwaysFailed { reason } => match lang {
            Lang::Ko => format!("-# ⚠️ 권한 저장 실패: {}", reason),
            Lang::En => format!("-# ⚠️ Permission save failed: {}", reason),
        },
        DisableReason::AutoDismissedByAlwaysChain { triggering_rule } => match lang {
            Lang::Ko => format!("-# 🔓 `{}` 등록으로 자동 취소됨", triggering_rule),
            Lang::En => format!("-# 🔓 Auto-dismissed by `{}`", triggering_rule),
        },
    };

    // tool_name은 라벨에 미포함이지만 향후 로깅 등에 활용 가능
    let _ = tool_name;

    let edit = EditMessage::new().content(label).components(vec![]);

    channel_id
        .edit_message(ctx, message_id, edit)
        .await
        .map_err(|e| PidoryError::Discord(Box::new(e)))?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn run_permission_handler(
    mut permission_rx: mpsc::Receiver<PermissionRequest>,
    ctx: Context,
    channel_id: ChannelId,
    pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    pending_question_groups: Arc<Mutex<HashMap<String, PendingQuestionGroup>>>,
    _owner_id: u64,
    thread_id: String,
    lang: Lang,
) {
    while let Some(perm_req) = permission_rx.recv().await {
        let triggered_by = perm_req.triggered_by;
        tracing::info!(thread_id = %thread_id, request_id = %perm_req.request_id, tool_name = %perm_req.tool_name, "permission request received from worker");

        if perm_req.tool_name == "AskUserQuestion" {
            let count = question_ui::question_count(&perm_req.input);
            if count <= 1 {
                // Single question — direct PendingPermission, no group needed
                let msg = question_ui::create_question_message(
                    &perm_req.input,
                    &perm_req.request_id,
                    triggered_by,
                    lang,
                );
                match channel_id.send_message(&ctx, msg).await {
                    Ok(sent) => {
                        let pending = PendingPermission {
                            response_tx: perm_req.response_tx,
                            tool_name: perm_req.tool_name,
                            message_id: sent.id,
                            thread_id: thread_id.clone(),
                            triggered_by,
                            input: Some(perm_req.input),
                            scope_override: None,
                        };
                        pending_permissions
                            .lock()
                            .await
                            .insert(perm_req.request_id, pending);
                    }
                    Err(e) => {
                        warn!("Failed to send question message: {}", e);
                        let _ = perm_req.response_tx.send(PermissionDecision::Deny);
                    }
                }
            } else {
                // Multi-question — register group first (before sending messages)
                // to avoid race where a fast user answers before the group exists.
                let group = PendingQuestionGroup {
                    response_tx: perm_req.response_tx,
                    input: perm_req.input.clone(),
                    answers: HashMap::new(),
                    answered: HashSet::new(),
                    total: count,
                    thread_id: thread_id.clone(),
                    triggered_by,
                };
                pending_question_groups
                    .lock()
                    .await
                    .insert(perm_req.request_id.clone(), group);

                // Send each question as a separate message
                let mut all_ok = true;
                for idx in 0..count {
                    let sub_id =
                        question_ui::make_sub_request_id(&perm_req.request_id, idx);
                    let msg = question_ui::create_question_message_for_index(
                        &perm_req.input,
                        idx,
                        &sub_id,
                        triggered_by,
                        lang,
                    );
                    match channel_id.send_message(&ctx, msg).await {
                        Ok(sent) => {
                            let (dummy_tx, _) =
                                tokio::sync::oneshot::channel::<PermissionDecision>();
                            let pending = PendingPermission {
                                response_tx: dummy_tx,
                                tool_name: perm_req.tool_name.clone(),
                                message_id: sent.id,
                                thread_id: thread_id.clone(),
                                triggered_by,
                                input: Some(perm_req.input.clone()),
                                scope_override: None,
                            };
                            pending_permissions.lock().await.insert(sub_id, pending);
                        }
                        Err(e) => {
                            warn!("Failed to send question message (q{}): {}", idx, e);
                            all_ok = false;
                            break;
                        }
                    }
                }

                if !all_ok {
                    // Clean up: remove group (to recover response_tx) and any sub-questions
                    if let Some(group) = pending_question_groups.lock().await.remove(&perm_req.request_id) {
                        let _ = group.response_tx.send(PermissionDecision::Deny);
                    }
                    let mut perms = pending_permissions.lock().await;
                    for idx in 0..count {
                        let sub_id =
                            question_ui::make_sub_request_id(&perm_req.request_id, idx);
                        perms.remove(&sub_id);
                    }
                }
            }
            continue;
        }

        let msg = create_permission_message(
            &perm_req.tool_name,
            &perm_req.input,
            &perm_req.request_id,
            perm_req.decision_reason.as_deref(),
            triggered_by,
            default_scope(), // P1.3 (#288) 에서 DB user_settings 조회로 교체
            lang,
        );

        let log_request_id = perm_req.request_id.clone();
        let log_tool_name = perm_req.tool_name.clone();
        match channel_id.send_message(&ctx, msg).await {
            Ok(sent) => {
                tracing::info!(thread_id = %thread_id, request_id = %log_request_id, tool_name = %log_tool_name, "permission message sent");
                let pending = PendingPermission {
                    response_tx: perm_req.response_tx,
                    tool_name: perm_req.tool_name,
                    message_id: sent.id,
                    thread_id: thread_id.clone(),
                    triggered_by,
                    input: Some(perm_req.input),
                    scope_override: None,
                };
                pending_permissions
                    .lock()
                    .await
                    .insert(perm_req.request_id, pending);
                tracing::info!(thread_id = %thread_id, request_id = %log_request_id, tool_name = %log_tool_name, "pending_permission inserted");
            }
            Err(e) => {
                tracing::info!(thread_id = %thread_id, request_id = %log_request_id, tool_name = %log_tool_name, "permission message send failed");
                warn!("Failed to send permission message: {}", e);
                // 전송 실패 시 deny
                let _ = perm_req.response_tx.send(PermissionDecision::Deny);
            }
        }
    }
}

/// dismiss_pending_by_tool 의 반환 타입.
pub(crate) struct DismissedEntry {
    pub request_id: String,
    pub message_id: MessageId,
    pub thread_id: String,
}

/// `thread_id` + `tool_name` 이 모두 일치하는 대기 permission 을 HashMap 에서 remove + response_tx 로 decision 전송.
/// `pending_permissions` 는 모든 세션이 공유하는 global 맵이므로 반드시 thread_id 로 격리해야 cross-session dismiss 를 막을 수 있다.
/// AskUserQuestion 은 제외 (sub-request id 패턴 `{rid}__q{idx}` 은 tool_name 이 "AskUserQuestion" 이므로 자연 배제).
/// 반환: 실제로 dismiss 된 entry 들의 메시지 메타정보 (buttons disable 용).
pub(crate) async fn dismiss_pending_by_tool(
    pending_permissions: &Arc<Mutex<HashMap<String, crate::PendingPermission>>>,
    thread_id: &str,
    tool_name: &str,
    decision: PermissionDecision,
    exclude_request_id: &str,
) -> Vec<DismissedEntry> {
    if tool_name == "AskUserQuestion" {
        return Vec::new();
    }

    let mut map = pending_permissions.lock().await;
    let matched_ids: Vec<String> = map
        .iter()
        .filter(|(rid, p)| {
            p.tool_name == tool_name
                && p.thread_id == thread_id
                && rid.as_str() != exclude_request_id
        })
        .map(|(rid, _)| rid.clone())
        .collect();

    let mut dismissed = Vec::with_capacity(matched_ids.len());
    for rid in matched_ids {
        if let Some(entry) = map.remove(&rid) {
            let _ = entry.response_tx.send(decision.clone());
            dismissed.push(DismissedEntry {
                request_id: rid,
                message_id: entry.message_id,
                thread_id: entry.thread_id,
            });
        }
    }
    dismissed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_settings::rule::RuleKind;

    // ── parse_permission_custom_id ────────────────────────────────────────────

    #[test]
    fn parse_perm_once() {
        let result = parse_permission_custom_id("perm:abc-123:once");
        assert_eq!(result, Some(("abc-123".to_string(), PermAction::Once)));
    }

    #[test]
    fn parse_perm_deny() {
        let result = parse_permission_custom_id("perm:abc-123:deny");
        assert_eq!(result, Some(("abc-123".to_string(), PermAction::Deny)));
    }

    #[test]
    fn parse_perm_scope_toggle() {
        let result = parse_permission_custom_id("perm:abc-123:scope:toggle");
        assert_eq!(result, Some(("abc-123".to_string(), PermAction::ScopeToggle)));
    }

    #[test]
    fn parse_perm_always_exact() {
        let result = parse_permission_custom_id("perm:abc-123:always:exact");
        assert_eq!(
            result,
            Some(("abc-123".to_string(), PermAction::AllowAlways(RuleKind::Exact)))
        );
    }

    #[test]
    fn parse_perm_always_tool() {
        let result = parse_permission_custom_id("perm:abc-123:always:tool");
        assert_eq!(
            result,
            Some(("abc-123".to_string(), PermAction::AllowAlways(RuleKind::Tool)))
        );
    }

    #[test]
    fn parse_perm_rid_with_colon_always_prefix() {
        // rid에 ':' 포함 — suffix strip 방식으로 정확히 보존해야 한다
        let result = parse_permission_custom_id("perm:rid-with:colon:always:prefix");
        assert_eq!(
            result,
            Some(("rid-with:colon".to_string(), PermAction::AllowAlways(RuleKind::Prefix)))
        );
    }

    #[test]
    fn parse_perm_invalid_tail_returns_none() {
        let result = parse_permission_custom_id("perm:abc:invalid");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_perm_wrong_prefix_returns_none() {
        let result = parse_permission_custom_id("not-perm:abc:once");
        assert_eq!(result, None);
    }

    /// Disabled 레이블 버튼의 custom_id 는 해석되지 않아야 한다 (review #298 s4).
    /// 향후 parse 규칙이 바뀌어도 이 ID 가 우연히 매칭되면 핸들러가 의도치 않은 분기로 빠짐.
    #[test]
    fn parse_perm_label_returns_none() {
        let result = parse_permission_custom_id(LABEL_BUTTON_CUSTOM_ID);
        assert_eq!(result, None);
    }

    // ── format_tool_input_summary ─────────────────────────────────────────────

    #[test]
    fn format_bash_summary() {
        let input = serde_json::json!({"command": "ls -la"});
        let result = format_tool_input_summary("Bash", &input);
        assert!(result.contains("ls -la"));
        assert!(result.contains("```"));
    }

    #[test]
    fn format_edit_summary() {
        let input = serde_json::json!({"file_path": "/tmp/foo.rs"});
        let result = format_tool_input_summary("Edit", &input);
        assert_eq!(result, "`/tmp/foo.rs`");
    }

    #[test]
    fn format_unknown_summary() {
        let input = serde_json::json!({});
        let result = format_tool_input_summary("Unknown", &input);
        assert_eq!(result, "");
    }

    #[test]
    fn format_webfetch_summary() {
        let input = serde_json::json!({"url": "https://example.com/page"});
        let result = format_tool_input_summary("WebFetch", &input);
        assert_eq!(result, "`https://example.com/page`");
    }

    #[test]
    fn format_websearch_summary() {
        let input = serde_json::json!({"query": "rust async tokio"});
        let result = format_tool_input_summary("WebSearch", &input);
        assert_eq!(result, "`rust async tokio`");
    }

    #[test]
    fn format_unknown_with_string_field() {
        let input = serde_json::json!({"some_field": "some value"});
        let result = format_tool_input_summary("UnknownTool", &input);
        assert_eq!(result, "`some value`");
    }

    #[test]
    fn format_unknown_with_long_string_field() {
        let long_str = "a".repeat(150);
        let input = serde_json::json!({"field": long_str});
        let result = format_tool_input_summary("UnknownTool", &input);
        assert_eq!(result, format!("`{}`", "a".repeat(100)));
    }

    // ── format_tool_input_summary: remaining tool variants ───────────────────

    #[test]
    fn format_write_summary() {
        let input = serde_json::json!({"file_path": "/src/main.rs"});
        let result = format_tool_input_summary("Write", &input);
        assert_eq!(result, "`/src/main.rs`");
    }

    #[test]
    fn format_read_summary() {
        let input = serde_json::json!({"file_path": "/etc/hosts"});
        let result = format_tool_input_summary("Read", &input);
        assert_eq!(result, "`/etc/hosts`");
    }

    #[test]
    fn format_grep_summary() {
        let input = serde_json::json!({"pattern": "fn main"});
        let result = format_tool_input_summary("Grep", &input);
        assert_eq!(result, "`fn main`");
    }

    #[test]
    fn format_glob_summary() {
        let input = serde_json::json!({"pattern": "**/*.rs"});
        let result = format_tool_input_summary("Glob", &input);
        assert_eq!(result, "`**/*.rs`");
    }

    #[test]
    fn format_bash_empty_command() {
        let input = serde_json::json!({"command": ""});
        let result = format_tool_input_summary("Bash", &input);
        // Empty command still produces code-fence block
        assert!(result.contains("```"));
    }

    #[test]
    fn format_bash_missing_command_key() {
        // If "command" key is absent, falls back to empty string
        let input = serde_json::json!({});
        let result = format_tool_input_summary("Bash", &input);
        assert!(result.contains("```"));
    }

    // ── dismiss_pending_by_tool ───────────────────────────────────────────────

    fn make_pending(
        tool_name: &str,
    ) -> (
        crate::PendingPermission,
        tokio::sync::oneshot::Receiver<PermissionDecision>,
    ) {
        make_pending_for_thread(tool_name, "thread-test")
    }

    fn make_pending_for_thread(
        tool_name: &str,
        thread_id: &str,
    ) -> (
        crate::PendingPermission,
        tokio::sync::oneshot::Receiver<PermissionDecision>,
    ) {
        let (tx, rx) = tokio::sync::oneshot::channel::<PermissionDecision>();
        let pending = crate::PendingPermission {
            response_tx: tx,
            tool_name: tool_name.to_string(),
            message_id: MessageId::new(12345),
            thread_id: thread_id.to_string(),
            triggered_by: UserId::new(99999),
            input: None,
            scope_override: None,
        };
        (pending, rx)
    }

    #[tokio::test]
    async fn dismiss_pending_by_tool_removes_matching_entries() {
        let map: Arc<Mutex<HashMap<String, crate::PendingPermission>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (p1, _rx1) = make_pending("WebFetch");
        let (p2, _rx2) = make_pending("WebFetch");
        let (p3, _rx3) = make_pending("Read");
        let (p4, _rx4) = make_pending("AskUserQuestion");

        {
            let mut m = map.lock().await;
            m.insert("1".to_string(), p1);
            m.insert("2".to_string(), p2);
            m.insert("3".to_string(), p3);
            m.insert("4".to_string(), p4);
        }

        let dismissed = dismiss_pending_by_tool(
            &map,
            "thread-test",
            "WebFetch",
            PermissionDecision::Allow,
            "nonexistent",
        )
        .await;

        assert_eq!(dismissed.len(), 2);

        let map_locked = map.lock().await;
        assert_eq!(map_locked.len(), 2);
        assert!(map_locked.contains_key("3"), "Read entry must survive");
        assert!(
            map_locked.contains_key("4"),
            "AskUserQuestion entry must survive"
        );

        let mut rids: Vec<&str> = dismissed.iter().map(|e| e.request_id.as_str()).collect();
        rids.sort();
        assert_eq!(rids, vec!["1", "2"]);
    }

    #[tokio::test]
    async fn dismiss_pending_by_tool_excludes_caller_request_id() {
        let map: Arc<Mutex<HashMap<String, crate::PendingPermission>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (p1, _rx1) = make_pending("WebFetch");
        let (p2, _rx2) = make_pending("WebFetch");

        {
            let mut m = map.lock().await;
            m.insert("1".to_string(), p1);
            m.insert("2".to_string(), p2);
        }

        let dismissed = dismiss_pending_by_tool(
            &map,
            "thread-test",
            "WebFetch",
            PermissionDecision::Allow,
            "1",
        )
        .await;

        assert_eq!(dismissed.len(), 1);
        assert_eq!(dismissed[0].request_id, "2");

        let map_locked = map.lock().await;
        assert!(
            map_locked.contains_key("1"),
            "Excluded entry (rid=1) must remain"
        );
        assert!(
            !map_locked.contains_key("2"),
            "Non-excluded entry (rid=2) must be removed"
        );
    }

    #[tokio::test]
    async fn dismiss_pending_by_tool_ignores_ask_user_question() {
        let map: Arc<Mutex<HashMap<String, crate::PendingPermission>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (p1, _rx1) = make_pending("AskUserQuestion");
        let (p2, _rx2) = make_pending("AskUserQuestion");

        {
            let mut m = map.lock().await;
            m.insert("1".to_string(), p1);
            m.insert("2".to_string(), p2);
        }

        let dismissed = dismiss_pending_by_tool(
            &map,
            "thread-test",
            "AskUserQuestion",
            PermissionDecision::Allow,
            "nonexistent",
        )
        .await;

        assert_eq!(
            dismissed.len(),
            0,
            "AskUserQuestion must trigger early-return"
        );

        let map_locked = map.lock().await;
        assert_eq!(
            map_locked.len(),
            2,
            "All AskUserQuestion entries must be untouched"
        );
    }

    #[tokio::test]
    async fn dismiss_pending_by_tool_sends_decision_via_response_tx() {
        let map: Arc<Mutex<HashMap<String, crate::PendingPermission>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (p1, rx1) = make_pending("WebFetch");

        {
            let mut m = map.lock().await;
            m.insert("1".to_string(), p1);
        }

        let dismissed = dismiss_pending_by_tool(
            &map,
            "thread-test",
            "WebFetch",
            PermissionDecision::AllowAlways {
                rule_kind: crate::claude_settings::rule::RuleKind::Exact,
                scope: crate::claude_settings::rule::Scope::Project,
            },
            "nonexistent",
        )
        .await;

        assert_eq!(dismissed.len(), 1);

        let decision = rx1.await.expect("response_tx must have fired");
        assert!(matches!(decision, PermissionDecision::AllowAlways { .. }));
    }

    /// Cross-session dismiss 방지: 다른 thread_id 의 pending 은 같은 tool_name 이어도 건드리지 않는다.
    /// `pending_permissions` 는 global 맵이므로 thread_id 필터 없이 dismiss 하면
    /// 세션 A 의 AlwaysAllow 클릭이 세션 B 의 pending 을 자동 Allow 시키는 버그가 발생한다.
    #[tokio::test]
    async fn dismiss_pending_by_tool_isolates_by_thread_id() {
        let map: Arc<Mutex<HashMap<String, crate::PendingPermission>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (p_a1, _rx_a1) = make_pending_for_thread("WebFetch", "thread-A");
        let (p_a2, _rx_a2) = make_pending_for_thread("WebFetch", "thread-A");
        let (p_b1, mut rx_b1) = make_pending_for_thread("WebFetch", "thread-B");
        let (p_b2, mut rx_b2) = make_pending_for_thread("WebFetch", "thread-B");

        {
            let mut m = map.lock().await;
            m.insert("a1".to_string(), p_a1);
            m.insert("a2".to_string(), p_a2);
            m.insert("b1".to_string(), p_b1);
            m.insert("b2".to_string(), p_b2);
        }

        // thread-A 에서 a1 을 AlwaysAllow 클릭 → a2 만 dismiss, thread-B 는 건드리지 않음.
        let dismissed = dismiss_pending_by_tool(
            &map,
            "thread-A",
            "WebFetch",
            PermissionDecision::Allow,
            "a1",
        )
        .await;

        assert_eq!(dismissed.len(), 1, "only thread-A's a2 must be dismissed");
        assert_eq!(dismissed[0].request_id, "a2");
        assert_eq!(dismissed[0].thread_id, "thread-A");

        let map_locked = map.lock().await;
        assert!(map_locked.contains_key("a1"), "excluded a1 must remain");
        assert!(!map_locked.contains_key("a2"), "a2 must be dismissed");
        assert!(map_locked.contains_key("b1"), "thread-B's b1 must remain");
        assert!(map_locked.contains_key("b2"), "thread-B's b2 must remain");
        drop(map_locked);

        // thread-B 의 response_tx 는 firing 되지 않았어야 한다.
        assert!(
            rx_b1.try_recv().is_err(),
            "thread-B's b1 must NOT receive a decision (cross-session leak)"
        );
        assert!(
            rx_b2.try_recv().is_err(),
            "thread-B's b2 must NOT receive a decision (cross-session leak)"
        );
    }

    // ── run_permission_handler: lifecycle — tx drop causes exit ──────────────

    #[tokio::test]
    async fn permission_handler_exits_when_sender_dropped() {
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::sync::{Mutex, mpsc};
        use crate::subprocess::permission::PermissionRequest;

        // We can't construct a real serenity Context, so we test the underlying
        // contract: `while let Some(perm_req) = permission_rx.recv().await` exits
        // when all senders are dropped. We verify this by driving the same loop
        // shape with a bare mpsc channel.
        let (tx, mut rx) = mpsc::channel::<PermissionRequest>(8);
        let pending: Arc<Mutex<HashMap<String, crate::PendingPermission>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Spawn a task that mimics the handler's loop termination behaviour.
        let handle = tokio::spawn(async move {
            while let Some(_req) = rx.recv().await {
                // would handle permission here
            }
            // exits when all senders dropped
        });

        drop(tx);

        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("handler-like task should exit within 1s")
            .expect("task must not panic");

        // Pending map remains empty since no messages were processed.
        assert!(pending.lock().await.is_empty());
    }

    // ── create_permission_message: 3 ActionRow 구조 검증 ─────────────────────
    //
    // serenity CreateMessage/CreateButton 필드는 private → serde_json::to_value로
    // 직렬화 후 JSON 구조로 검증한다.

    fn msg_to_json(msg: CreateMessage) -> serde_json::Value {
        serde_json::to_value(msg).expect("CreateMessage must be serializable")
    }

    fn get_rows(v: &serde_json::Value) -> &serde_json::Value {
        v.get("components").expect("components field must exist")
    }

    fn row_buttons(rows: &serde_json::Value, idx: usize) -> &serde_json::Value {
        rows.as_array()
            .expect("components must be array")[idx]
            .get("components")
            .expect("row must have components")
    }

    fn btn_custom_id(buttons: &serde_json::Value, idx: usize) -> &str {
        buttons.as_array()
            .expect("buttons must be array")[idx]
            .get("custom_id")
            .and_then(|v| v.as_str())
            .expect("button must have custom_id")
    }

    fn btn_style(buttons: &serde_json::Value, idx: usize) -> u64 {
        buttons.as_array()
            .expect("buttons must be array")[idx]
            .get("style")
            .and_then(|v| v.as_u64())
            .expect("button must have style")
    }

    /// Bash + Project scope → Row1=2버튼, Row2=[label, Exact, Prefix, Tool, scope]=5버튼
    #[test]
    fn create_permission_message_bash_project_two_rows() {
        let input = serde_json::json!({"command": "npm test"});
        let msg = create_permission_message(
            "Bash",
            &input,
            "rid-001",
            None,
            UserId::new(12345),
            Scope::Project,
            Lang::Ko,
        );
        let v = msg_to_json(msg);
        let rows = get_rows(&v);
        assert_eq!(rows.as_array().unwrap().len(), 2, "Bash+Project → 2 ActionRow");

        // Row 1: once + deny = 2 버튼
        let row1 = row_buttons(rows, 0);
        assert_eq!(row1.as_array().unwrap().len(), 2, "Row1: once + deny");
        assert!(btn_custom_id(row1, 0).ends_with(":once"), "Row1[0] = once");
        assert!(btn_custom_id(row1, 1).ends_with(":deny"), "Row1[1] = deny");

        // Row 2: [label, Exact, Prefix, Tool, scope-toggle] = 5 버튼
        let row2 = row_buttons(rows, 1);
        assert_eq!(row2.as_array().unwrap().len(), 5, "Row2: label+Exact+Prefix+Tool+scope");
        assert_eq!(btn_custom_id(row2, 0), LABEL_BUTTON_CUSTOM_ID, "Row2[0] = disabled label");
        assert!(btn_custom_id(row2, 1).ends_with(":always:exact"), "Row2[1] = always:exact");
        assert!(btn_custom_id(row2, 2).ends_with(":always:prefix"), "Row2[2] = always:prefix");
        assert!(btn_custom_id(row2, 3).ends_with(":always:tool"), "Row2[3] = always:tool");
        assert!(btn_custom_id(row2, 4).ends_with(":scope:toggle"), "Row2[4] = scope:toggle");
    }

    /// Row 2 첫 버튼은 disabled — Discord 가 클릭 이벤트 보내지 않아 핸들러 호출 안 됨
    #[test]
    fn create_permission_message_label_button_is_disabled() {
        let input = serde_json::json!({"command": "ls"});
        let msg = create_permission_message(
            "Bash",
            &input,
            "rid-disabled",
            None,
            UserId::new(12345),
            Scope::Project,
            Lang::Ko,
        );
        let v = msg_to_json(msg);
        let rows = get_rows(&v);
        let row2 = row_buttons(rows, 1);
        let label_btn = &row2.as_array().unwrap()[0];
        assert_eq!(
            label_btn.get("disabled").and_then(|v| v.as_bool()),
            Some(true),
            "label button must be disabled"
        );
    }

    /// WebFetch + Project scope (정상 URL) → Row2=[label, Domain, Tool, scope] = 4 버튼
    #[test]
    fn create_permission_message_webfetch_project_two_always_buttons() {
        let input = serde_json::json!({"url": "https://api.example.com/v1"});
        let msg = create_permission_message(
            "WebFetch",
            &input,
            "rid-002",
            None,
            UserId::new(12345),
            Scope::Project,
            Lang::Ko,
        );
        let v = msg_to_json(msg);
        let rows = get_rows(&v);
        assert_eq!(rows.as_array().unwrap().len(), 2, "WebFetch+Project → 2 ActionRow");

        let row2 = row_buttons(rows, 1);
        assert_eq!(row2.as_array().unwrap().len(), 4, "Row2: label+Domain+Tool+scope");
        assert_eq!(btn_custom_id(row2, 0), LABEL_BUTTON_CUSTOM_ID);
        assert!(btn_custom_id(row2, 1).ends_with(":always:domain"), "Row2[1] = always:domain");
        assert!(btn_custom_id(row2, 2).ends_with(":always:tool"), "Row2[2] = always:tool");
        assert!(btn_custom_id(row2, 3).ends_with(":scope:toggle"), "Row2[3] = scope:toggle");
    }

    /// Grep + Project scope → Row2=[label, Tool, scope] = 3 버튼
    #[test]
    fn create_permission_message_grep_project_one_always_button() {
        let input = serde_json::json!({"pattern": "fn main"});
        let msg = create_permission_message(
            "Grep",
            &input,
            "rid-003",
            None,
            UserId::new(12345),
            Scope::Project,
            Lang::Ko,
        );
        let v = msg_to_json(msg);
        let rows = get_rows(&v);
        assert_eq!(rows.as_array().unwrap().len(), 2, "Grep+Project → 2 ActionRow");

        let row2 = row_buttons(rows, 1);
        assert_eq!(row2.as_array().unwrap().len(), 3, "Row2: label+Tool+scope");
        assert_eq!(btn_custom_id(row2, 0), LABEL_BUTTON_CUSTOM_ID);
        assert!(btn_custom_id(row2, 1).ends_with(":always:tool"), "Row2[1] = always:tool");
        assert!(btn_custom_id(row2, 2).ends_with(":scope:toggle"), "Row2[2] = scope:toggle");
    }

    /// Bash + Global scope → 헤더에 "🌐 global" + "⚠️" 포함, scope 버튼 Primary(style=1)
    #[test]
    fn create_permission_message_bash_global_header_contains_global_warning() {
        let input = serde_json::json!({"command": "npm install"});
        let msg = create_permission_message(
            "Bash",
            &input,
            "rid-004",
            None,
            UserId::new(99999),
            Scope::Global,
            Lang::Ko,
        );
        let v = msg_to_json(msg);
        let content = v.get("content").and_then(|v| v.as_str()).unwrap_or("");
        assert!(content.contains("🌐 global"), "헤더에 🌐 global 포함");
        assert!(content.contains("⚠️"), "헤더에 ⚠️ 포함");
        assert!(content.contains("모든 프로젝트에 적용됨"), "경고 문구 포함");

        // Row 2 마지막 버튼 (scope toggle) Primary(style=1) — Global on
        let rows = get_rows(&v);
        let row2 = row_buttons(rows, 1);
        let last_idx = row2.as_array().unwrap().len() - 1;
        // Discord: Primary=1, Secondary=2, Success=3, Danger=4
        assert_eq!(btn_style(row2, last_idx), 1, "Global scope → Primary(1) 버튼");
    }

    /// Project scope → scope 버튼 Secondary(style=2)
    #[test]
    fn create_permission_message_project_scope_button_secondary() {
        let input = serde_json::json!({"command": "ls"});
        let msg = create_permission_message(
            "Bash",
            &input,
            "rid-005",
            None,
            UserId::new(12345),
            Scope::Project,
            Lang::Ko,
        );
        let v = msg_to_json(msg);
        let rows = get_rows(&v);
        let row2 = row_buttons(rows, 1);
        let last_idx = row2.as_array().unwrap().len() - 1;
        assert_eq!(btn_style(row2, last_idx), 2, "Project scope → Secondary(2) 버튼");
    }
}
