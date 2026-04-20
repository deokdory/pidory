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
use crate::handler::formatter::inline_code;
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
        "<@{}> 🔒 {} {}\n{}{}",
        triggered_by, inline_code(tool_name), lang.permission_request_label(), summary, reason
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
        _ => format!("-# {} — {}", inline_code(tool_name), chosen_action),
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
                    input: None,
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
            PermissionDecision::AlwaysAllow,
            "nonexistent",
        )
        .await;

        assert_eq!(dismissed.len(), 1);

        let decision = rx1.await.expect("response_tx must have fired");
        assert_eq!(decision, PermissionDecision::AlwaysAllow);
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
}
