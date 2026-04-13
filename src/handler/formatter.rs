use comfy_table::{CellAlignment, ContentArrangement, Table, presets};
use poise::serenity_prelude as serenity;
use serenity::{CreateAllowedMentions, CreateMessage};
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
                                    parts.push(convert_markdown_tables(&cleaned));
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
                    if is_noise_tool(tool_name) && !result.is_error {
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

fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 5 && trimmed.starts_with('|') && trimmed.ends_with('|')
}

fn is_separator_row(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 5 || !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return false;
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    inner.split('|').all(|cell| {
        let c = cell.trim();
        if c.is_empty() {
            return false;
        }
        let stripped = c.trim_start_matches(':').trim_end_matches(':');
        stripped.len() >= 3 && stripped.chars().all(|ch| ch == '-')
    })
}

fn parse_row_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let inner = &trimmed[1..trimmed.len() - 1];
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

fn parse_alignment(sep_cell: &str) -> CellAlignment {
    let c = sep_cell.trim();
    let left_colon = c.starts_with(':');
    let right_colon = c.ends_with(':');
    match (left_colon, right_colon) {
        (true, true) => CellAlignment::Center,
        (false, true) => CellAlignment::Right,
        _ => CellAlignment::Left,
    }
}

fn render_table(header: &[String], alignments: &[CellAlignment], rows: &[Vec<String>]) -> String {
    let mut table = Table::new();
    table.load_preset(presets::UTF8_FULL_CONDENSED);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_width(60);

    let header_cells: Vec<comfy_table::Cell> = header
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let alignment = alignments.get(i).copied().unwrap_or(CellAlignment::Left);
            comfy_table::Cell::new(h).set_alignment(alignment)
        })
        .collect();
    table.set_header(header_cells);

    for row in rows {
        let cells: Vec<comfy_table::Cell> = row
            .iter()
            .enumerate()
            .map(|(i, val)| {
                let alignment = alignments.get(i).copied().unwrap_or(CellAlignment::Left);
                comfy_table::Cell::new(val).set_alignment(alignment)
            })
            .collect();
        table.add_row(cells);
    }

    table.to_string()
}

pub(crate) fn convert_markdown_tables(text: &str) -> String {
    // Fast path: no pipe character means no possible table
    if !text.contains('|') {
        return text.to_string();
    }

    let lines: Vec<&str> = text.lines().collect();
    let mut result = String::new();
    let mut in_code_block = false;
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Track code block fences — skip processing inside them
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            result.push_str(line);
            result.push('\n');
            i += 1;
            continue;
        }

        if in_code_block {
            result.push_str(line);
            result.push('\n');
            i += 1;
            continue;
        }

        // Check if this could be the start of a markdown table:
        // line i must be a table row, line i+1 must be a separator row
        if is_table_row(line) && i + 1 < lines.len() && is_separator_row(lines[i + 1]) {
            // Collect header + separator + 1+ data rows
            let header_line = line;
            let sep_line = lines[i + 1];

            // Collect data rows
            let mut data_rows: Vec<&str> = Vec::new();
            let mut j = i + 2;
            while j < lines.len() && is_table_row(lines[j]) {
                data_rows.push(lines[j]);
                j += 1;
            }

            if !data_rows.is_empty() {
                // If any cell contains an escaped pipe, skip conversion and passthrough
                let has_escaped_pipe = header_line.contains("\\|")
                    || data_rows.iter().any(|r| r.contains("\\|"));
                if has_escaped_pipe {
                    result.push_str(line);
                    result.push('\n');
                    i += 1;
                    continue;
                }

                // Parse header cells
                let header_cells = parse_row_cells(header_line);

                // Parse alignments from separator
                let sep_cells = parse_row_cells(sep_line);
                let alignments: Vec<CellAlignment> =
                    sep_cells.iter().map(|c| parse_alignment(c)).collect();

                // Parse data rows
                let parsed_rows: Vec<Vec<String>> =
                    data_rows.iter().map(|r| parse_row_cells(r)).collect();

                let rendered = render_table(&header_cells, &alignments, &parsed_rows);
                result.push_str("```\n");
                result.push_str(&rendered);
                result.push('\n');
                result.push_str("```\n");

                i = j;
                continue;
            }
        }

        result.push_str(line);
        result.push('\n');
        i += 1;
    }

    // Remove trailing newline added by the loop if the original didn't end with one
    if !text.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
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
    fn format_response_skips_noise_tool_results() {
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

        // Write 성공 result도 필터링
        let events_write = vec![
            StreamEvent::Assistant {
                content: vec![ContentBlock::ToolUse {
                    id: "tool-2".into(),
                    name: "Write".into(),
                    input: serde_json::json!({"file_path": "/tmp/out.txt", "content": "hello"}),
                }],
                session_id: "s1".into(),
            },
            StreamEvent::User {
                tool_results: vec![ToolResult {
                    tool_use_id: "tool-2".into(),
                    content: "write success output".into(),
                    is_error: false,
                }],
                session_id: "s1".into(),
            },
        ];

        let (result_write, _) = format_response(&events_write, Lang::Ko);
        assert!(
            !result_write.contains("write success output"),
            "Successful Write tool results should be filtered out, but got: {}",
            result_write
        );

        // Write 에러 result는 표시
        let events_write_err = vec![
            StreamEvent::Assistant {
                content: vec![ContentBlock::ToolUse {
                    id: "tool-3".into(),
                    name: "Write".into(),
                    input: serde_json::json!({"file_path": "/tmp/out.txt", "content": "hello"}),
                }],
                session_id: "s1".into(),
            },
            StreamEvent::User {
                tool_results: vec![ToolResult {
                    tool_use_id: "tool-3".into(),
                    content: "permission denied error".into(),
                    is_error: true,
                }],
                session_id: "s1".into(),
            },
        ];

        let (result_write_err, _) = format_response(&events_write_err, Lang::Ko);
        assert!(
            result_write_err.contains("permission denied error"),
            "Write tool error results should be shown, but got: {}",
            result_write_err
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

    #[test]
    fn format_tool_use_multi_edit() {
        let input = serde_json::json!({"file_path": "/src/main.rs"});
        let result = format_tool_use("MultiEdit", &input);
        assert!(result.contains("**MultiEdit**"));
        assert!(result.contains("/src/main.rs"));
    }

    #[test]
    fn format_tool_use_web_search() {
        let input = serde_json::json!({"query": "rust async await"});
        let result = format_tool_use("WebSearch", &input);
        assert!(result.contains("**WebSearch**"));
        assert!(result.contains("rust async await"));
    }

    #[test]
    fn format_tool_use_web_fetch() {
        let input = serde_json::json!({"url": "https://example.com"});
        let result = format_tool_use("WebFetch", &input);
        assert!(result.contains("**WebFetch**"));
        assert!(result.contains("https://example.com"));
    }

    #[test]
    fn format_tool_use_todo_write() {
        let input = serde_json::json!({"todos": []});
        let result = format_tool_use("TodoWrite", &input);
        assert!(result.contains("**TodoWrite**"));
    }

    #[test]
    fn is_noise_tool_returns_true_for_filtered_tools() {
        for name in &["Read", "Grep", "Glob", "Write", "Edit", "MultiEdit", "WebSearch", "WebFetch", "TodoWrite"] {
            assert!(is_noise_tool(Some(name)), "{} should be a noise tool", name);
        }
    }

    #[test]
    fn is_noise_tool_returns_false_for_non_filtered_tools() {
        for name in &["Bash", "Agent", "CustomTool"] {
            assert!(!is_noise_tool(Some(name)), "{} should not be a noise tool", name);
        }
        assert!(!is_noise_tool(None));
    }

    #[test]
    fn convert_basic_table() {
        let input = "| a | b |\n|---|---|\n| 1 | 2 |";
        let result = convert_markdown_tables(input);
        assert!(result.starts_with("```\n"), "result should start with code fence: {:?}", result);
        assert!(result.ends_with("\n```"), "result should end with code fence: {:?}", result);
        assert!(!result.contains("| a |"), "original pipe row should not be present");
        assert!(result.contains('─'), "result should contain unicode box drawing chars");
    }

    #[test]
    fn convert_table_with_alignment() {
        let input = "| left | center | right |\n|:---|:---:|---:|\n| a | b | c |";
        let result = convert_markdown_tables(input);
        assert!(result.contains("```"), "result should contain code block");
        assert!(result.contains("left"), "header 'left' should be present");
        assert!(result.contains('a'), "cell value 'a' should be present");
    }

    #[test]
    fn convert_table_preserves_surrounding_text() {
        let input = "before\n| a | b |\n|---|---|\n| 1 | 2 |\nafter";
        let result = convert_markdown_tables(input);
        assert!(result.starts_with("before\n```\n"), "result should start with surrounding text then code fence: {:?}", result);
        assert!(result.contains("```\nafter"), "result should end with code fence then surrounding text: {:?}", result);
    }

    #[test]
    fn convert_table_skips_code_block() {
        let input = "```\n| a | b |\n|---|---|\n| 1 | 2 |\n```";
        let result = convert_markdown_tables(input);
        assert_eq!(result, input, "table inside code block should not be converted");
    }

    #[test]
    fn convert_table_no_separator_passthrough() {
        let input = "| not | a | table |";
        let result = convert_markdown_tables(input);
        assert_eq!(result, input, "line without separator row should pass through unchanged");
    }

    #[test]
    fn convert_table_cjk_chars() {
        let input = "| 이름 | 값 |\n|---|---|\n| 한글 | 테스트 |";
        let result = convert_markdown_tables(input);
        assert!(result.contains("```"), "result should contain code block");
        assert!(result.contains("이름"), "header '이름' should be present");
        assert!(result.contains("한글"), "cell '한글' should be present");
        assert!(result.contains("테스트"), "cell '테스트' should be present");
    }

    #[test]
    fn convert_table_empty_cells() {
        let input = "| a | b |\n|---|---|\n|  | 2 |";
        let result = convert_markdown_tables(input);
        assert!(result.contains("```"), "result should contain code block");
        assert!(result.contains('2'), "non-empty cell value should be present");
    }

    #[test]
    fn convert_multiple_tables() {
        let input = "| a | b |\n|---|---|\n| 1 | 2 |\n\ntext\n\n| c | d |\n|---|---|\n| 3 | 4 |";
        let result = convert_markdown_tables(input);
        let fence_count = result.matches("```").count();
        assert_eq!(fence_count, 4, "two tables should produce 4 code fences, got {}: {:?}", fence_count, result);
        assert!(result.contains("text"), "surrounding text should be preserved");
    }

    #[test]
    fn convert_table_fallback_on_malformed() {
        let input = "| a | b |\n|---|---|";
        let result = convert_markdown_tables(input);
        assert_eq!(result, input, "header+separator without data rows should not be converted");
    }

    #[test]
    fn convert_table_single_pipe_no_panic() {
        let input = "|\n|---|\n| x |";
        let result = convert_markdown_tables(input);
        assert_eq!(result, input, "single pipe header should not be recognized as table row");
    }

    #[test]
    fn convert_table_escaped_pipe_passthrough() {
        let input = "| a | b \\| c |\n|---|---|\n| 1 | 2 |";
        let result = convert_markdown_tables(input);
        assert_eq!(result, input, "table with escaped pipe should pass through unchanged");
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

/// 성공 시 Discord에 result를 표시하지 않는 도구 목록
pub fn is_noise_tool(tool_name: Option<&str>) -> bool {
    matches!(tool_name, Some("Read" | "Grep" | "Glob" | "Write" | "Edit" | "MultiEdit" | "WebSearch" | "WebFetch" | "TodoWrite"))
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
        "MultiEdit" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("-# 🔧 **MultiEdit** {}", file_path)
        }
        "WebSearch" => {
            let query = input
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("-# 🔧 **WebSearch** {}", query)
        }
        "WebFetch" => {
            let url = input
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("-# 🔧 **WebFetch** {}", url)
        }
        "TodoWrite" => {
            format!("-# 🔧 **TodoWrite**")
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

    let is_edit = tool_name == Some("Edit") || tool_name == Some("Write") || tool_name == Some("MultiEdit");
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

/// Discord에 메시지를 전송한다. reply_to가 있으면 reply로 전송, 없으면 일반 전송.
/// 향후 reply 기능 활성화 시 사용.
#[allow(dead_code)]
pub async fn send_reply(
    ctx: &serenity::Context,
    channel_id: serenity::ChannelId,
    content: &str,
    reply_to: Option<serenity::MessageId>,
) -> Result<serenity::Message, serenity::Error> {
    if let Some(msg_id) = reply_to {
        let message = CreateMessage::new()
            .content(content)
            .reference_message((channel_id, msg_id))
            // TODO(reply-activate): fail_if_not_exists(false) 추가 필요.
            // serenity API 확인 후 reply 기능 활성화 시 처리.
            .allowed_mentions(CreateAllowedMentions::new());
        channel_id.send_message(ctx, message).await
    } else {
        channel_id.say(ctx, content).await
    }
}
