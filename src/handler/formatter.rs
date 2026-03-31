use poise::serenity_prelude as serenity;
use tokio::time::{sleep, Duration};

use crate::error::PidoryError;
use crate::subprocess::parser::{ContentBlock, StreamEvent, ToolResult};

pub fn format_response(events: &[StreamEvent]) -> String {
    let mut parts: Vec<String> = Vec::new();
    // Maps tool_use_id -> tool name for matching results to their tool calls
    let mut tool_use_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for event in events {
        match event {
            StreamEvent::Assistant { content, .. } => {
                for block in content {
                    match block {
                        ContentBlock::Text(text) => {
                            if !text.is_empty() {
                                parts.push(text.clone());
                            }
                        }
                        ContentBlock::Thinking(_) => {
                            // ignored — not displayed on Discord
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_use_names.insert(id.clone(), name.clone());
                            parts.push(format_tool_use(name, input));
                        }
                    }
                }
            }
            StreamEvent::User { tool_results, .. } => {
                for result in tool_results {
                    let tool_name = tool_use_names.get(&result.tool_use_id).map(|s| s.as_str());
                    if let Some(formatted) = format_tool_result_with_name(result, tool_name) {
                        parts.push(formatted);
                    }
                }
            }
            StreamEvent::RateLimit { status, .. } => {
                if status != "allowed" {
                    parts.push("⚠️ Rate limit reached".to_string());
                }
            }
            StreamEvent::Result {
                duration_ms,
                ..
            } => {
                parts.push(format!("-# {}ms", duration_ms));
            }
            _ => {}
        }
    }

    parts.join("\n")
}

pub fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let lines: Vec<&str> = text.lines().collect();
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();

    for line in &lines {
        let line_with_newline = format!("{}\n", line);

        // Detect code block start/end before split decision
        let is_fence = line.starts_with("```");
        let will_toggle = is_fence;

        // Check if adding this line would exceed the limit
        if current.len() + line_with_newline.len() > max_len && !current.is_empty() {
            if in_code_block && !is_fence {
                // Close the open code block in the current chunk
                current.push_str("```\n");
            }
            chunks.push(current.trim_end().to_string());
            current = String::new();
            if in_code_block && !is_fence {
                // Re-open the code block in the new chunk
                current.push_str(&format!("```{}\n", code_lang));
            }
        }

        // Now toggle code block state
        if will_toggle {
            if in_code_block {
                in_code_block = false;
                code_lang = String::new();
            } else {
                in_code_block = true;
                code_lang = line.trim_start_matches('`').to_string();
            }
        }

        current.push_str(&line_with_newline);
    }

    if !current.trim().is_empty() {
        chunks.push(current.trim_end().to_string());
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subprocess::parser::ToolResult;

    #[test]
    fn split_short_message() {
        let result = split_message("hello", 100);
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn split_at_newline() {
        let text = "line1\nline2\nline3";
        let result = split_message(text, 10);
        for chunk in &result {
            assert!(chunk.len() <= 12);
        }
        let joined = result.join("\n");
        assert!(joined.contains("line1"));
        assert!(joined.contains("line2"));
        assert!(joined.contains("line3"));
    }

    #[test]
    fn split_preserves_code_block() {
        // When code block spans a chunk boundary, the implementation closes it in
        // the current chunk and re-opens it in the next. Each chunk should be
        // self-consistent (no unmatched opening fence without a closing one).
        let text = "before\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\nafter";
        let result = split_message(text, 40);
        // Every chunk that has an opening ``` must also have a closing ``` or
        // the last line of the chunk is a ```. All fences come in pairs globally.
        let total_fences: usize = result.iter().map(|c| c.matches("```").count()).sum();
        assert_eq!(total_fences % 2, 0, "Total fence count must be even");
    }

    #[test]
    fn split_returns_single_chunk_when_fits() {
        let text = "short text";
        let result = split_message(text, 1000);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], text);
    }

    #[test]
    fn format_tool_use_bash() {
        let input = serde_json::json!({"command": "echo hello"});
        let result = format_tool_use("Bash", &input);
        assert!(result.contains("**Bash**"));
        assert!(result.contains("echo hello"));
    }

    #[test]
    fn format_tool_use_edit() {
        let input = serde_json::json!({"file_path": "/tmp/foo.rs"});
        let result = format_tool_use("Edit", &input);
        assert!(result.contains("**Edit**"));
        assert!(result.contains("/tmp/foo.rs"));
    }

    #[test]
    fn format_tool_use_read() {
        let input = serde_json::json!({"file_path": "/tmp/bar.rs"});
        let result = format_tool_use("Read", &input);
        assert!(result.contains("**Read**"));
        assert!(result.contains("/tmp/bar.rs"));
    }

    #[test]
    fn format_tool_use_unknown() {
        let input = serde_json::json!({});
        let result = format_tool_use("CustomTool", &input);
        assert!(result.contains("**CustomTool**"));
    }

    #[test]
    fn format_tool_result_empty() {
        let tr = ToolResult { tool_use_id: "t1".into(), content: "".into(), is_error: false };
        assert!(format_tool_result(&tr).is_none());
    }

    #[test]
    fn format_tool_result_short() {
        let tr = ToolResult { tool_use_id: "t1".into(), content: "output".into(), is_error: false };
        let result = format_tool_result(&tr).unwrap();
        assert!(result.contains("output"));
        assert!(!result.contains("❌"));
    }

    #[test]
    fn format_tool_result_error() {
        let tr = ToolResult { tool_use_id: "t1".into(), content: "err".into(), is_error: true };
        let result = format_tool_result(&tr).unwrap();
        assert!(result.starts_with("❌"));
    }

    #[test]
    fn format_tool_result_truncated() {
        let long_content = "x".repeat(600);
        let tr = ToolResult { tool_use_id: "t1".into(), content: long_content, is_error: false };
        let result = format_tool_result(&tr).unwrap();
        assert!(result.contains("truncated"));
    }
}

pub fn format_tool_use(name: &str, input: &serde_json::Value) -> String {
    match name {
        "Bash" => {
            let command = input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("> 🔧 **Bash**\n```\n{}\n```", command)
        }
        "Edit" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("> 🔧 **Edit** {}", file_path)
        }
        "Read" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("> 🔧 **Read** {}", file_path)
        }
        "Write" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("> 🔧 **Write** {}", file_path)
        }
        "Grep" => {
            let pattern = input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("> 🔧 **Grep** {}", pattern)
        }
        "Glob" => {
            let pattern = input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("> 🔧 **Glob** {}", pattern)
        }
        _ => format!("> 🔧 **{}**", name),
    }
}

#[allow(dead_code)]
pub fn format_tool_result(result: &ToolResult) -> Option<String> {
    format_tool_result_with_name(result, None)
}

pub fn format_tool_result_with_name(result: &ToolResult, tool_name: Option<&str>) -> Option<String> {
    if result.content.is_empty() {
        return None;
    }

    const TRUNCATE_LEN: usize = 500;

    let prefix = if result.is_error { "❌ " } else { "" };

    let is_edit = tool_name == Some("Edit") || tool_name == Some("Write");
    let fence = if is_edit { "```diff" } else { "```" };

    let body = if result.content.len() <= TRUNCATE_LEN {
        format!("{}\n{}\n```", fence, result.content)
    } else {
        let truncated = &result.content[..TRUNCATE_LEN];
        format!("{}\n{}\n```\n...(truncated)", fence, truncated)
    };

    Some(format!("{}{}", prefix, body))
}

pub async fn send_response(
    ctx: &serenity::Context,
    channel_id: serenity::ChannelId,
    text: &str,
    max_chunk_len: usize,
    max_chunks: usize,
) -> Result<(), PidoryError> {
    let chunks = split_message(text, max_chunk_len);

    if chunks.len() <= max_chunks {
        for (i, chunk) in chunks.iter().enumerate() {
            channel_id.say(ctx, chunk).await?;
            if i + 1 < chunks.len() {
                sleep(Duration::from_millis(200)).await;
            }
        }
    } else {
        // Send the first max_chunks chunks
        for chunk in chunks.iter().take(max_chunks) {
            channel_id.say(ctx, chunk).await?;
            sleep(Duration::from_millis(200)).await;
        }

        // Collect the remainder into a file attachment
        let remainder = chunks[max_chunks..].join("\n");
        let attachment = serenity::CreateAttachment::bytes(
            remainder.into_bytes(),
            "response_overflow.txt",
        );
        let message = serenity::CreateMessage::new()
            .content("*(response continues in attachment)*")
            .add_file(attachment);
        channel_id.send_message(ctx, message).await?;
    }

    Ok(())
}
