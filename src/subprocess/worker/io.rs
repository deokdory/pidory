use poise::serenity_prelude::{ChannelId, Context, CreateMessage, MessageFlags};

use crate::handler::formatter;
use crate::subprocess::session_manager::ReplyContext;

pub(super) async fn say_silent_chunked(ctx: &Context, channel_id: &ChannelId, text: &str) {
    let chunks = formatter::split_message(text, 2000);
    for chunk in chunks {
        let msg = CreateMessage::new()
            .content(chunk)
            .flags(MessageFlags::SUPPRESS_NOTIFICATIONS);
        if let Err(e) = channel_id.send_message(ctx, msg).await {
            tracing::warn!(%channel_id, "Failed to send bg message to Discord: {}", e);
        }
    }
}

// ─── T6: Common JSON builder helpers ───────────────────────────────────────

pub(super) fn build_user_message_json(content: &str, downloaded_files: &[String], reply_context: Option<&ReplyContext>) -> String {
    let mut text = String::new();

    // 1. reply context — system-reminder로 신뢰 경계 분리, </system-reminder> 인젝션 방지
    if let Some(reply) = reply_context {
        // Sanitize untrusted reply content to prevent prompt injection
        let safe_content = reply.original_content
            .replace("</system-reminder>", "[/system-reminder]")
            .replace("<system-reminder>", "[system-reminder]");
        let safe_author = reply.original_author_name
            .replace("</system-reminder>", "[/system-reminder]")
            .replace("<system-reminder>", "[system-reminder]");
        text.push_str(&format!(
            "<system-reminder>\n이 메시지는 다음 메시지에 대한 reply(답장)입니다:\n[원본 작성자: {}]\n{}\n</system-reminder>\n\n",
            safe_author, safe_content
        ));
    }

    // 2. 첨부파일 system-reminder
    if !downloaded_files.is_empty() {
        let paths: String = downloaded_files
            .iter()
            .map(|p| {
                let relative = if let Some(idx) = p.find(".pidory/") {
                    &p[idx..]
                } else {
                    p.as_str()
                };
                format!("- {relative}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        text.push_str(&format!(
            "<system-reminder>\n사용자가 파일을 첨부했습니다. 프로젝트 상대 경로로 접근하세요:\n{paths}\n</system-reminder>\n\n"
        ));
    }

    // 3. 사용자 메시지
    text.push_str(content);

    let json = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    });
    format!("{}\n", json)
}

pub(super) fn build_interrupt_json() -> String {
    let msg = serde_json::json!({
        "type": "control_request",
        "request_id": format!("interrupt_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()),
        "request": {"subtype": "interrupt"}
    });
    format!("{}\n", msg)
}

#[cfg(test)]
mod tests {
    use super::{build_user_message_json, build_interrupt_json};
    use crate::subprocess::session_manager::ReplyContext;

    // ── build_user_message_json ──────────────────────────────────────────────

    #[test]
    fn user_message_json_basic_structure() {
        let out = build_user_message_json("hello", &[], None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        let content = v["message"]["content"].as_array().expect("content is array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "hello");
    }

    #[test]
    fn user_message_json_special_chars_escaped() {
        let out = build_user_message_json("hello \"world\"", &[], None);
        // Must round-trip through JSON without error and preserve the value
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["message"]["content"][0]["text"], "hello \"world\"");
    }

    #[test]
    fn user_message_json_korean_and_emoji() {
        let out = build_user_message_json("안녕 🎉", &[], None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["message"]["content"][0]["text"], "안녕 🎉");
    }

    #[test]
    fn user_message_json_empty_string() {
        let out = build_user_message_json("", &[], None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["message"]["content"][0]["text"], "");
    }

    #[test]
    fn user_message_json_ends_with_newline() {
        let out = build_user_message_json("hello", &[], None);
        assert!(out.ends_with('\n'), "output must end with newline");
    }

    #[test]
    fn build_message_no_attachments() {
        let out = build_user_message_json("hello", &[], None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"][0]["type"], "text");
        assert_eq!(v["message"]["content"][0]["text"], "hello");
    }

    #[test]
    fn build_message_with_attachments() {
        let files = vec!["/project/.pidory/downloads/123/456_file.py".to_string()];
        let out = build_user_message_json("hello", &files, None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        assert!(text.contains("<system-reminder>"), "must contain system-reminder tag");
        assert!(text.contains("hello"), "must contain original content");
    }

    #[test]
    fn build_message_attachment_paths_relative() {
        let files = vec!["/project/.pidory/downloads/123/456_file.py".to_string()];
        let out = build_user_message_json("hello", &files, None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        assert!(text.contains(".pidory/downloads/123/456_file.py"), "must contain relative path");
        assert!(!text.contains("/project/.pidory/"), "must not contain absolute path prefix");
    }

    #[test]
    fn build_message_multiple_attachments() {
        let files = vec![
            "/project/.pidory/downloads/123/a.png".to_string(),
            "/project/.pidory/downloads/123/b.csv".to_string(),
        ];
        let out = build_user_message_json("hello", &files, None);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        assert!(text.contains(".pidory/downloads/123/a.png"), "must list first file");
        assert!(text.contains(".pidory/downloads/123/b.csv"), "must list second file");
    }

    #[test]
    fn build_message_with_reply_context() {
        let reply_ctx = ReplyContext {
            original_content: "This is the original message".to_string(),
            original_author_name: "Alice".to_string(),
        };
        let out = build_user_message_json("follow-up question", &[], Some(&reply_ctx));
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        assert!(text.contains("<system-reminder>"), "must contain system-reminder tag");
        assert!(text.contains("reply(답장)"), "must mention reply");
        assert!(text.contains("Alice"), "must contain original author name");
        assert!(text.contains("This is the original message"), "must contain original content");
        assert!(text.contains("follow-up question"), "must contain user message");
    }

    #[test]
    fn build_message_reply_context_plus_attachments() {
        let reply_ctx = ReplyContext {
            original_content: "Original".to_string(),
            original_author_name: "Bob".to_string(),
        };
        let files = vec!["/project/.pidory/downloads/123/file.py".to_string()];
        let out = build_user_message_json("question", &files, Some(&reply_ctx));
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        // Must have both system-reminder blocks
        let reminder_count = text.matches("<system-reminder>").count();
        assert_eq!(reminder_count, 2, "must have two system-reminder blocks (reply + attachments)");
        // Reply context should come first
        let reply_pos = text.find("reply(답장)").expect("reply context");
        let file_pos = text.find(".pidory/downloads").expect("attachment");
        assert!(reply_pos < file_pos, "reply context must come before attachments");
    }

    #[test]
    fn build_message_reply_context_empty_original() {
        // Test that empty original_content is still injected (unlike Discord behavior)
        // The filtering happens in resolve_reply_context, not build_user_message_json
        let reply_ctx = ReplyContext {
            original_content: "".to_string(),
            original_author_name: "Charlie".to_string(),
        };
        let out = build_user_message_json("question", &[], Some(&reply_ctx));
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        // Even with empty content, the system-reminder should be present
        assert!(text.contains("<system-reminder>"), "system-reminder must be present");
        assert!(text.contains("Charlie"), "author name must be included");
    }

    #[test]
    fn build_message_reply_context_special_chars() {
        let reply_ctx = ReplyContext {
            original_content: r#"Line 1: "quoted" text\nLine 2: <tag>content</tag>"#.to_string(),
            original_author_name: "User\\Name".to_string(),
        };
        let out = build_user_message_json("follow-up", &[], Some(&reply_ctx));
        // Must be valid JSON even with special characters
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let text = v["message"]["content"][0]["text"].as_str().expect("text field");
        // Special characters should be preserved
        assert!(text.contains(r#""quoted""#), "should preserve quoted text");
        assert!(text.contains("User\\Name"), "should preserve backslash in name");
    }

    // ── build_interrupt_json ─────────────────────────────────────────────────

    #[test]
    fn interrupt_json_type_is_control_request() {
        let out = build_interrupt_json();
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["type"], "control_request");
    }

    #[test]
    fn interrupt_json_subtype_is_interrupt() {
        let out = build_interrupt_json();
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["request"]["subtype"], "interrupt");
    }

    #[test]
    fn interrupt_json_request_id_prefix() {
        let out = build_interrupt_json();
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        let rid = v["request_id"].as_str().expect("request_id is string");
        assert!(rid.starts_with("interrupt_"), "request_id must start with 'interrupt_', got: {rid}");
    }

    #[test]
    fn interrupt_json_ends_with_newline() {
        let out = build_interrupt_json();
        assert!(out.ends_with('\n'), "output must end with newline");
    }
}
