use poise::serenity_prelude as serenity;
use tokio::time::{sleep, Duration};

use crate::error::PidoryError;
use crate::handler::file_attach;
use crate::i18n::Lang;
use crate::subprocess::parser::{ContentBlock, StreamEvent, ToolResult};

pub fn format_response(events: &[StreamEvent], lang: Lang) -> (String, Vec<String>) {
    let mut parts: Vec<String> = Vec::new();
    let mut file_paths: Vec<String> = Vec::new();
    // Maps tool_use_id -> tool name for matching results to their tool calls
    let mut tool_use_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for event in events {
        match event {
            StreamEvent::Assistant { content, .. } => {
                for block in content {
                    match block {
                        ContentBlock::Text(text) => {
                            if !text.is_empty() {
                                let (cleaned, paths) = file_attach::extract_file_markers(text);
                                file_paths.extend(paths);
                                if !cleaned.is_empty() {
                                    parts.push(cleaned);
                                }
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
                    if matches!(tool_name, Some("Read" | "Grep" | "Glob")) && !result.is_error {
                        continue;
                    }
                    if let Some(formatted) = format_tool_result_with_name(result, tool_name, lang) {
                        parts.push(formatted);
                    }
                }
            }
            StreamEvent::RateLimit { status, .. } => {
                if status == "rate_limited" {
                    parts.push(lang.rate_limit_reached().to_string());
                } else if status != "allowed" && !status.is_empty() {
                    tracing::warn!(status, "Unknown rate limit status");
                }
            }
            _ => {}
        }
    }

    (parts.join("\n"), file_paths)
}

pub fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.chars().count() <= max_len {
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
        if current.chars().count() + line_with_newline.chars().count() > max_len && !current.is_empty() {
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

        // Force-split if a single line caused current to exceed max_len
        if current.chars().count() > max_len {
            let chars: Vec<char> = current.chars().collect();
            let effective_len = if in_code_block { max_len.saturating_sub(10) } else { max_len };
            let chunk_iter: Vec<String> = chars.chunks(effective_len)
                .map(|c| c.iter().collect::<String>())
                .filter(|s| !s.trim().is_empty())
                .collect();

            for (i, s) in chunk_iter.iter().enumerate() {
                let mut chunk = s.clone();
                // Close the code block in all but the last chunk
                if in_code_block && i < chunk_iter.len() - 1 {
                    chunk.push_str("\n```");
                }
                chunks.push(chunk);
            }

            // Preserve code block state — do NOT reset in_code_block or code_lang
            current = String::new();
            if in_code_block {
                current = format!("```{}\n", code_lang);
            }
            continue;
        }
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
            assert!(chunk.chars().count() <= 12);
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
    fn split_force_split_inside_code_block_preserves_fences() {
        // A code block containing a line longer than max_len should be force-split
        // such that every resulting chunk is valid Markdown (fences come in pairs).
        let inner = "x".repeat(2500);
        let text = format!("```rust\n{}\n```", inner);
        let result = split_message(&text, 1900);

        // Every chunk must have an even number of ``` occurrences
        for (i, chunk) in result.iter().enumerate() {
            let fence_count = chunk.matches("```").count();
            assert_eq!(
                fence_count % 2, 0,
                "Chunk {} has unbalanced fences ({} fences): {:?}",
                i, fence_count, &chunk[..chunk.len().min(80)]
            );
        }

        // The total content must contain the language hint
        let joined = result.join("\n");
        assert!(joined.contains("rust"), "Language hint should be present in output");
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
        assert!(format_tool_result_with_name(&tr, None, Lang::default()).is_none());
    }

    #[test]
    fn format_tool_result_short() {
        let tr = ToolResult { tool_use_id: "t1".into(), content: "output".into(), is_error: false };
        let result = format_tool_result_with_name(&tr, None, Lang::default()).unwrap();
        assert!(result.contains("output"));
        assert!(!result.contains("❌"));
    }

    #[test]
    fn format_tool_result_error() {
        let tr = ToolResult { tool_use_id: "t1".into(), content: "err".into(), is_error: true };
        let result = format_tool_result_with_name(&tr, None, Lang::default()).unwrap();
        assert!(result.contains("❌"));
    }

    #[test]
    fn format_tool_result_truncated() {
        let long_content = "x".repeat(600);
        let tr = ToolResult { tool_use_id: "t1".into(), content: long_content, is_error: false };
        let result = format_tool_result_with_name(&tr, None, Lang::Ko).unwrap();
        assert!(result.contains("잘림"));
        let result_en = format_tool_result_with_name(&tr, None, Lang::En).unwrap();
        assert!(result_en.contains("truncated"));
    }

    #[test]
    fn split_korean_multibyte_fits_in_char_limit() {
        // 700 Korean characters = 2100 bytes, but only 700 chars — fits within 1900 char limit
        let text: String = "가".repeat(700);
        let result = split_message(&text, 1900);
        assert_eq!(result.len(), 1, "700 Korean chars should fit in a single 1900-char chunk");
    }

    #[test]
    fn split_single_long_line_exceeding_max_len() {
        // A single line of 2500 chars with no newlines must be force-split
        let text = "a".repeat(2500);
        let result = split_message(&text, 1900);
        assert_eq!(result.len(), 2, "2500-char line should split into exactly 2 chunks at 1900 limit");
        for chunk in &result {
            assert!(
                chunk.chars().count() <= 1900,
                "Each chunk must be within the 1900-char limit, got {} chars",
                chunk.chars().count()
            );
        }
    }

    #[test]
    fn format_tool_use_bash_truncates_long_command() {
        let long_command = "x".repeat(2000);
        let result = format_tool_use("Bash", &serde_json::json!({"command": long_command}));
        assert!(
            result.chars().count() <= 1900,
            "Formatted Bash tool_use should fit within Discord limit, got {} chars",
            result.chars().count()
        );
        assert!(result.contains("…"), "Truncated command should contain the ellipsis indicator");
    }

    #[test]
    fn format_response_skips_read_grep_glob_results() {
        use crate::subprocess::parser::{ContentBlock, StreamEvent, ToolResult};

        let events = vec![
            StreamEvent::Assistant {
                content: vec![ContentBlock::ToolUse {
                    id: "tool-1".into(),
                    name: "Read".into(),
                    input: serde_json::json!({"file_path": "/tmp/secret.txt"}),
                }],
                session_id: "s1".into(),
            },
            StreamEvent::User {
                tool_results: vec![ToolResult {
                    tool_use_id: "tool-1".into(),
                    content: "file contents here".into(),
                    is_error: false,
                }],
                session_id: "s1".into(),
            },
        ];

        let (result, _files) = format_response(&events, Lang::Ko);
        assert!(
            !result.contains("file contents here"),
            "Successful Read tool results should be filtered out, but got: {}",
            result
        );
    }

    #[test]
    fn format_cost_zero_returns_empty() {
        assert_eq!(format_cost(0.0), "");
    }

    #[test]
    fn format_cost_positive_formats_with_leading_space() {
        assert_eq!(format_cost(0.05), " $0.05");
    }

    #[test]
    fn format_cost_rounds_to_two_decimal_places() {
        assert_eq!(format_cost(1.234), " $1.23");
    }

    #[test]
    fn format_cost_negative_returns_empty() {
        assert_eq!(format_cost(-0.01), "");
    }

    #[test]
    fn format_tokens_zero() {
        assert_eq!(format_tokens(0, 0), "");
    }

    #[test]
    fn format_tokens_small() {
        assert_eq!(format_tokens(100, 50), " 150 tok");
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(25000, 1200), " 26.2k tok");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens(900000, 200000), " 1.1M tok");
    }
}

pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let minutes = ms / 60_000;
        let seconds = (ms % 60_000) / 1000;
        format!("{}m{}s", minutes, seconds)
    }
}

pub fn format_cost(usd: f64) -> String {
    if usd <= 0.0 {
        String::new()
    } else {
        format!(" ${:.2}", usd)
    }
}

pub fn format_tokens(input: u64, output: u64) -> String {
    let total = input + output;
    if total == 0 {
        return String::new();
    }
    let formatted = if total >= 1_000_000 {
        format!("{:.1}M", total as f64 / 1_000_000.0)
    } else if total >= 1_000 {
        format!("{:.1}k", total as f64 / 1_000.0)
    } else {
        format!("{}", total)
    };
    format!(" {} tok", formatted)
}

/// Bash command max display length. Discord message limit (2000 chars) minus
/// markdown overhead (~100 chars) for a safe margin.
const BASH_COMMAND_DISPLAY_LIMIT: usize = 1800;

pub fn format_tool_use(name: &str, input: &serde_json::Value) -> String {
    match name {
        "Bash" => {
            let command = input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let display = if command.chars().count() > BASH_COMMAND_DISPLAY_LIMIT {
                let s: String = command.chars().take(BASH_COMMAND_DISPLAY_LIMIT).collect();
                format!("{}…", s)
            } else {
                command.to_string()
            };
            format!("-# 🔧 **Bash**\n```\n{}\n```", display)
        }
        "Edit" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("-# 🔧 **Edit** {}", file_path)
        }
        "Read" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("-# 🔧 **Read** {}", file_path)
        }
        "Write" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("-# 🔧 **Write** {}", file_path)
        }
        "Grep" => {
            let pattern = input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("-# 🔧 **Grep** {}", pattern)
        }
        "Glob" => {
            let pattern = input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("-# 🔧 **Glob** {}", pattern)
        }
        _ => format!("-# 🔧 **{}**", name),
    }
}

pub fn format_tool_result_with_name(result: &ToolResult, tool_name: Option<&str>, lang: Lang) -> Option<String> {
    if result.content.is_empty() {
        return None;
    }

    const TRUNCATE_LEN: usize = 500;

    let is_short = !result.content.contains('\n') && result.content.chars().count() <= 200;

    if is_short {
        let prefix = if result.is_error { "❌ " } else { "" };
        return Some(format!("-# {}{}", prefix, result.content));
    }

    let prefix = if result.is_error { "❌ " } else { "" };

    let is_edit = tool_name == Some("Edit") || tool_name == Some("Write");
    let fence = if is_edit { "```diff" } else { "```" };

    let body = if result.content.chars().count() <= TRUNCATE_LEN {
        format!("{}\n{}\n```", fence, result.content)
    } else {
        let truncated: String = result.content.chars().take(TRUNCATE_LEN).collect();
        format!("{}\n{}\n```\n{}", fence, truncated, lang.truncated_suffix())
    };

    Some(format!("{}{}", prefix, body))
}

pub async fn send_response(
    ctx: &serenity::Context,
    channel_id: serenity::ChannelId,
    text: &str,
    max_chunk_len: usize,
    max_chunks: usize,
    lang: Lang,
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
            .content(lang.response_continues())
            .add_file(attachment);
        channel_id.send_message(ctx, message).await?;
    }

    Ok(())
}
