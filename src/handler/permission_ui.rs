#![allow(dead_code)]

use poise::serenity_prelude::{
    ButtonStyle, ChannelId, Context, CreateActionRow, CreateButton, CreateMessage, EditMessage,
    MessageId,
};

use crate::error::PidoryError;

pub fn create_permission_message(
    tool_name: &str,
    input: &serde_json::Value,
    request_id: &str,
    decision_reason: Option<&str>,
    owner_id: u64,
) -> CreateMessage {
    let summary = format_tool_input_summary(tool_name, input);
    let reason = decision_reason
        .map(|r| format!("\n> {}", r))
        .unwrap_or_default();
    let content = format!(
        "<@{}> 🔒 **{}** 실행 허가 요청\n{}{}",
        owner_id, tool_name, summary, reason
    );

    let allow_btn = CreateButton::new(format!("perm:{}:allow", request_id))
        .label("Allow")
        .style(ButtonStyle::Success)
        .emoji('✅');
    let always_btn = CreateButton::new(format!("perm:{}:always", request_id))
        .label("Always Allow")
        .style(ButtonStyle::Success)
        .emoji('🔓');
    let deny_btn = CreateButton::new(format!("perm:{}:deny", request_id))
        .label("Deny")
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
) -> Result<(), PidoryError> {
    let label = match chosen_action {
        "allow" => format!("-# ✅ {} — Allowed", tool_name),
        "always" => format!("-# 🔓 {} — Always Allowed", tool_name),
        "deny" => format!("-# ❌ {} — Denied", tool_name),
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
}
