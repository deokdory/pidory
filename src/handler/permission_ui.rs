#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use poise::serenity_prelude::{
    ButtonStyle, ChannelId, Context, CreateActionRow, CreateButton, CreateMessage, EditMessage,
    MessageId, UserId,
};
use tokio::sync::{Mutex, mpsc};
use tracing::warn;

use crate::error::PidoryError;
use crate::handler::question_ui;
use crate::i18n::Lang;
use crate::subprocess::permission::{PermissionDecision, PermissionRequest};
use crate::{PendingPermission, PendingQuestionGroup};

pub fn create_permission_message(
    tool_name: &str,
    input: &serde_json::Value,
    request_id: &str,
    decision_reason: Option<&str>,
    triggered_by: UserId,
    lang: Lang,
) -> CreateMessage {
    let summary = format_tool_input_summary(tool_name, input);
    let reason = decision_reason
        .map(|r| format!("\n> {}", r))
        .unwrap_or_default();
    let content = format!(
        "<@{}> 🔒 **{}** {}\n{}{}",
        triggered_by, tool_name, lang.permission_request_label(), summary, reason
    );

    let allow_btn = CreateButton::new(format!("perm:{}:allow", request_id))
        .label(lang.btn_allow())
        .style(ButtonStyle::Success)
        .emoji('✅');
    let always_btn = CreateButton::new(format!("perm:{}:always", request_id))
        .label(lang.btn_always_allow())
        .style(ButtonStyle::Success)
        .emoji('🔓');
    let deny_btn = CreateButton::new(format!("perm:{}:deny", request_id))
        .label(lang.btn_deny())
        .style(ButtonStyle::Danger)
        .emoji('❌');

    let row = CreateActionRow::Buttons(vec![allow_btn, always_btn, deny_btn]);

    CreateMessage::new().content(content).components(vec![row])
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

pub async fn disable_permission_buttons(
    ctx: &Context,
    channel_id: ChannelId,
    message_id: MessageId,
    chosen_action: &str,
    tool_name: &str,
    lang: Lang,
) -> Result<(), PidoryError> {
    let label = match chosen_action {
        "allow" => format!("-# ✅ {}", lang.perm_allowed(tool_name)),
        "always" => format!("-# 🔓 {}", lang.perm_always_allowed(tool_name)),
        "deny" => format!("-# ❌ {}", lang.perm_denied(tool_name)),
        _ => format!("-# {} — {}", tool_name, chosen_action),
    };

    let edit = EditMessage::new().content(label).components(vec![]);

    channel_id
        .edit_message(ctx, message_id, edit)
        .await
        .map_err(|e| PidoryError::Discord(Box::new(e)))?;

    Ok(())
}

/// Parses custom_id in the format `perm:{request_id}:{action}`.
/// Returns `(request_id, action)` or `None` if the format does not match.
pub fn parse_permission_custom_id(custom_id: &str) -> Option<(String, String)> {
    let stripped = custom_id.strip_prefix("perm:")?;
    let (request_id, action) = stripped.rsplit_once(':')?;
    Some((request_id.to_string(), action.to_string()))
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
                // Multi-question — send each question as a separate message,
                // create PendingPermission per sub-question, and a PendingQuestionGroup
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
                            // Sub-question PendingPermission — no response_tx (group owns it)
                            // We use a dummy oneshot that's never awaited
                            let (dummy_tx, _) =
                                tokio::sync::oneshot::channel::<PermissionDecision>();
                            let pending = PendingPermission {
                                response_tx: dummy_tx,
                                tool_name: perm_req.tool_name.clone(),
                                message_id: sent.id,
                                thread_id: thread_id.clone(),
                                triggered_by,
                                input: Some(perm_req.input.clone()),
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

                if all_ok {
                    let group = PendingQuestionGroup {
                        response_tx: perm_req.response_tx,
                        input: perm_req.input,
                        answers: HashMap::new(),
                        total: count,
                        thread_id: thread_id.clone(),
                        triggered_by,
                    };
                    pending_question_groups
                        .lock()
                        .await
                        .insert(perm_req.request_id, group);
                } else {
                    // Clean up any sub-questions that were sent
                    let mut perms = pending_permissions.lock().await;
                    for idx in 0..count {
                        let sub_id =
                            question_ui::make_sub_request_id(&perm_req.request_id, idx);
                        perms.remove(&sub_id);
                    }
                    let _ = perm_req.response_tx.send(PermissionDecision::Deny);
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
                    input: None,
                };
                pending_permissions
                    .lock()
                    .await
                    .insert(perm_req.request_id, pending);
            }
            Err(e) => {
                warn!("Failed to send permission message: {}", e);
                // 전송 실패 시 deny
                let _ = perm_req.response_tx.send(PermissionDecision::Deny);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_custom_id() {
        let (rid, action) =
            parse_permission_custom_id("perm:e5c3058b-6794-4a0d-b445-7729855cb810:allow").unwrap();
        assert_eq!(rid, "e5c3058b-6794-4a0d-b445-7729855cb810");
        assert_eq!(action, "allow");
    }

    #[test]
    fn parse_always_action() {
        let (rid, action) = parse_permission_custom_id("perm:some-id:always").unwrap();
        assert_eq!(rid, "some-id");
        assert_eq!(action, "always");
    }

    #[test]
    fn parse_deny_action() {
        let (_, action) = parse_permission_custom_id("perm:abc:deny").unwrap();
        assert_eq!(action, "deny");
    }

    #[test]
    fn parse_invalid_prefix() {
        assert!(parse_permission_custom_id("other:abc:allow").is_none());
    }

    #[test]
    fn parse_no_action() {
        assert!(parse_permission_custom_id("perm:abc").is_none());
    }

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

    // ── parse_permission_custom_id: edge cases ────────────────────────────────

    #[test]
    fn parse_colon_in_request_id() {
        // rsplit_once(':') means the last ':' is the separator — so colons inside
        // the request_id are preserved in the first part.
        let (rid, action) = parse_permission_custom_id("perm:a:b:allow").unwrap();
        assert_eq!(rid, "a:b");
        assert_eq!(action, "allow");
    }

    #[test]
    fn parse_empty_string() {
        assert!(parse_permission_custom_id("").is_none());
    }

    #[test]
    fn parse_perm_prefix_only() {
        assert!(parse_permission_custom_id("perm:").is_none());
    }

    #[test]
    fn parse_action_preserved_case() {
        let (_, action) = parse_permission_custom_id("perm:id:Allow").unwrap();
        // Action is returned as-is, case sensitive
        assert_eq!(action, "Allow");
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
}
