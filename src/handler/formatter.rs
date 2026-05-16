use comfy_table::{CellAlignment, ContentArrangement, Table, presets};
use poise::serenity_prelude as serenity;
use serenity::{CreateAllowedMentions, CreateEmbed, CreateMessage};
use tokio::time::{sleep, Duration};

use crate::error::PidoryError;
use crate::handler::file_attach;
use crate::i18n::Lang;
use crate::subprocess::parser::{ContentBlock, StreamEvent, ToolResult};

/// Wraps `name` in a Discord inline code span, stripping any backticks from
/// the name itself to prevent the span from being broken.
pub fn inline_code(name: &str) -> String {
    if name.contains('`') {
        format!("`{}`", name.replace('`', ""))
    } else {
        format!("`{}`", name)
    }
}

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
                                    let table_converted = convert_markdown_tables(&cleaned);
                                    parts.push(convert_html_details(&table_converted));
                                }
                            }
                        }
                        ContentBlock::Thinking(_) => {
                            // ignored — not displayed on Discord
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_use_names.insert(id.clone(), name.clone());
                            let formatted = format_tool_use(name, input);
                            if !formatted.is_empty() {
                                parts.push(formatted);
                            }
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

pub(crate) fn convert_html_details(text: &str) -> String {
    // Fast path: no details tag means nothing to convert
    if !text.contains("<details") {
        return text.to_owned();
    }

    let lines: Vec<&str> = text.lines().collect();
    let mut result = String::new();
    let mut in_code_block = false;

    let mut in_details = false;
    let mut header: Option<String> = None;
    let mut buffer: Vec<String> = Vec::new();

    for line in &lines {
        // Track code block fences — skip processing inside them
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            if in_details {
                buffer.push(line.to_string());
            } else {
                result.push_str(line);
                result.push('\n');
            }
            continue;
        }

        if in_code_block {
            if in_details {
                buffer.push(line.to_string());
            } else {
                result.push_str(line);
                result.push('\n');
            }
            continue;
        }

        if !in_details && line.contains("<details") {
            // Check for compact single-line form: <details...>...</details>
            if line.contains("</details>") {
                // Preserve text before <details
                let tag_pos = line.find("<details").unwrap();
                let prefix = line[..tag_pos].trim();
                if !prefix.is_empty() {
                    result.push_str(prefix);
                    result.push('\n');
                }

                // Extract summary if present
                let compact_header = if line.contains("<summary>") && line.contains("</summary>") {
                    let after_open = line.find("<summary>").map(|i| i + "<summary>".len());
                    let before_close = line.find("</summary>");
                    if let (Some(start), Some(end)) = (after_open, before_close)
                        && start <= end
                    {
                        let content = &line[start..end];
                        Some(format!("**{}**", content.trim().replace("**", "")))
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Extract content between </summary> and </details>
                let content_start = line
                    .find("</summary>")
                    .map(|i| i + "</summary>".len())
                    .unwrap_or_else(|| line.find('>').map(|i| i + 1).unwrap_or(line.len()));
                let content_end = line.find("</details>").unwrap_or(line.len());
                let content = if content_start <= content_end {
                    line[content_start..content_end].trim()
                } else {
                    ""
                };

                if let Some(h) = compact_header {
                    result.push_str(&h);
                    result.push('\n');
                }
                if !content.is_empty() {
                    result.push_str("> ");
                    result.push_str(content);
                    result.push('\n');
                }

                // Preserve text after </details>
                let close_pos = line.find("</details>").unwrap();
                let suffix = line[close_pos + "</details>".len()..].trim();
                if !suffix.is_empty() {
                    result.push_str(suffix);
                    result.push('\n');
                }
                continue;
            }

            // Multi-line details block starts — preserve prefix text
            let tag_pos = line.find("<details").unwrap();
            let prefix = line[..tag_pos].trim();
            if !prefix.is_empty() {
                result.push_str(prefix);
                result.push('\n');
            }
            in_details = true;
            header = None;
            buffer = Vec::new();
            continue;
        }

        if in_details {
            if line.contains("</details>") {
                // Flush buffer to result
                if let Some(h) = header.take() {
                    result.push_str(&h);
                    result.push('\n');
                }
                for buf_line in &buffer {
                    if buf_line.is_empty() {
                        result.push('>');
                        result.push('\n');
                    } else {
                        result.push_str("> ");
                        result.push_str(buf_line);
                        result.push('\n');
                    }
                }
                // Preserve text after </details>
                let close_pos = line.find("</details>").unwrap();
                let suffix = line[close_pos + "</details>".len()..].trim();
                if !suffix.is_empty() {
                    result.push_str(suffix);
                    result.push('\n');
                }
                buffer = Vec::new();
                in_details = false;
                continue;
            }

            if line.contains("<summary>") && line.contains("</summary>") {
                let after_open = line.find("<summary>").map(|i| i + "<summary>".len());
                let before_close = line.find("</summary>");
                if let (Some(start), Some(end)) = (after_open, before_close)
                    && start <= end
                {
                    let content = &line[start..end];
                    header = Some(format!("**{}**", content.trim().replace("**", "")));
                    continue;
                }
            }

            // Regular content line inside details
            buffer.push(line.to_string());
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }

    // Malformed: still inside details at EOF — return original text
    if in_details {
        return text.to_owned();
    }

    // Remove trailing newline added by the loop if the original didn't end with one
    if !text.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
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
        format!("${:.2}", usd)
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
    format!("{} tok", formatted)
}

/// 성공 시 Discord에 result를 표시하지 않는 도구 목록
pub fn is_noise_tool(tool_name: Option<&str>) -> bool {
    matches!(tool_name, Some("Read" | "Grep" | "Glob" | "Write" | "Edit" | "MultiEdit" | "WebSearch" | "WebFetch" | "TodoWrite"))
}

fn todo_status_icon(status: &str) -> &'static str {
    match status {
        "completed" => "✅",
        "in_progress" => "🔄",
        _ => "⬜",
    }
}

fn todo_embed_color(todos: &[&serde_json::Value]) -> u32 {
    let has_in_progress = todos.iter().any(|t| {
        t.get("status").and_then(|s| s.as_str()) == Some("in_progress")
    });
    let all_completed = todos.iter().all(|t| {
        t.get("status").and_then(|s| s.as_str()) == Some("completed")
    });

    if all_completed {
        0x2ECC71u32
    } else if has_in_progress {
        0x3498DBu32
    } else {
        0x95A5A6u32
    }
}

fn escape_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '~' | '*' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn render_todo_line(todo: &serde_json::Value) -> String {
    let status = todo.get("status").and_then(|s| s.as_str()).unwrap_or("pending");
    let content = todo.get("content").and_then(|s| s.as_str()).unwrap_or("");
    let icon = todo_status_icon(status);
    match status {
        "completed" => format!("{} ~~{}~~", icon, escape_markdown(content)),
        "in_progress" => format!("{} **{}**", icon, escape_markdown(content)),
        _ => format!("{} {}", icon, escape_markdown(content)),
    }
}

pub fn format_todo_embed(input: &serde_json::Value) -> Option<CreateEmbed> {
    let todos = input.get("todos")?.as_array()?;
    if todos.is_empty() {
        return None;
    }

    let todos_refs: Vec<&serde_json::Value> = todos.iter().collect();
    let color = todo_embed_color(&todos_refs);

    let done = todos_refs
        .iter()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("completed"))
        .count();
    let total = todos_refs.len();

    // Separate into three buckets preserving original order
    let in_progress: Vec<&serde_json::Value> = todos_refs
        .iter()
        .copied()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("in_progress"))
        .collect();
    let pending: Vec<&serde_json::Value> = todos_refs
        .iter()
        .copied()
        .filter(|t| {
            !matches!(
                t.get("status").and_then(|s| s.as_str()),
                Some("completed") | Some("in_progress")
            )
        })
        .collect();
    let completed: Vec<&serde_json::Value> = todos_refs
        .iter()
        .copied()
        .filter(|t| t.get("status").and_then(|s| s.as_str()) == Some("completed"))
        .collect();

    // Fold completed items: show at most last 3 + "+N more completed" suffix
    const COMPLETED_SHOW: usize = 3;
    let (shown_completed, hidden_completed) = if completed.len() > COMPLETED_SHOW {
        let hidden = completed.len() - COMPLETED_SHOW;
        (&completed[hidden..], hidden)
    } else {
        (completed.as_slice(), 0)
    };

    // Build ordered lines: in_progress → pending → completed (folded)
    let mut lines: Vec<String> = Vec::new();
    for t in &in_progress {
        lines.push(render_todo_line(t));
    }
    for t in &pending {
        lines.push(render_todo_line(t));
    }
    for t in shown_completed {
        lines.push(render_todo_line(t));
    }
    if hidden_completed > 0 {
        lines.push(format!("  ... +{} more completed", hidden_completed));
    }

    // Build description with 4096-char hard truncation
    const MAX_DESC: usize = 4096;
    let mut description = String::new();
    let mut truncated_count = 0usize;
    let total_lines = lines.len();

    for (i, line) in lines.iter().enumerate() {
        let candidate = if description.is_empty() {
            line.clone()
        } else {
            format!("{}\n{}", description, line)
        };

        if candidate.chars().count() <= MAX_DESC {
            description = candidate;
        } else {
            truncated_count = total_lines - i;
            break;
        }
    }

    if truncated_count > 0 {
        let suffix = format!("\n... +{} more items", truncated_count);
        let available = MAX_DESC.saturating_sub(suffix.chars().count());
        if description.chars().count() > available {
            description = description.chars().take(available).collect();
        }
        description.push_str(&suffix);
    }

    let description = format!("\n{}\n", description);

    let embed = CreateEmbed::new()
        .color(color)
        .title(format!("Tasks · {}/{}", done, total))
        .description(description);

    Some(embed)
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
            String::new()  // embed로 처리됨, 일반 메시지 불필요
        }
        "Skill" => {
            let skill_name = input
                .get("skill")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if skill_name.is_empty() {
                "-# 🔧 **Skill**".to_string()
            } else {
                format!("-# 🔧 **Skill** {}", skill_name)
            }
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
    mention_cache: &crate::handler::mention::MentionCache,
    guild_id: Option<serenity::GuildId>,
    roster_snapshot: Option<&crate::mention::roster::RosterSnapshot>,
    scope: Option<&std::collections::HashSet<serenity::UserId>>,
) -> Result<(), PidoryError> {
    // mention 치환 + 화이트리스트 추출 (roster 경로 우선)
    let (processed_text, whitelist) =
        crate::handler::mention::parse_and_replace(text, guild_id, mention_cache, ctx, roster_snapshot, scope).await;

    let chunks = split_message(&processed_text, max_chunk_len);

    let am = || {
        serenity::CreateAllowedMentions::new()
            .everyone(false)
            .all_roles(false)
            .users(whitelist.clone())
    };

    if chunks.len() <= max_chunks {
        for (i, chunk) in chunks.iter().enumerate() {
            let msg = serenity::CreateMessage::new()
                .content(chunk)
                .allowed_mentions(am());
            channel_id.send_message(ctx, msg).await?;
            if i + 1 < chunks.len() {
                sleep(Duration::from_millis(200)).await;
            }
        }
    } else {
        // Send the first max_chunks chunks
        for chunk in chunks.iter().take(max_chunks) {
            let msg = serenity::CreateMessage::new()
                .content(chunk)
                .allowed_mentions(am());
            channel_id.send_message(ctx, msg).await?;
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
            .add_file(attachment)
            .allowed_mentions(am());
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
        assert_eq!(format_cost(0.05), "$0.05");
    }

    #[test]
    fn format_cost_rounds_to_two_decimal_places() {
        assert_eq!(format_cost(1.234), "$1.23");
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
        assert_eq!(format_tokens(100, 50), "150 tok");
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(25000, 1200), "26.2k tok");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens(900000, 200000), "1.1M tok");
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
        // TodoWrite returns empty string — handled via embed, no inline message needed
        assert!(result.is_empty());
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

    // --- format_todo_embed & helper tests ---

    #[test]
    fn test_todo_status_icon() {
        assert_eq!(todo_status_icon("completed"), "✅");
        assert_eq!(todo_status_icon("in_progress"), "🔄");
        assert_eq!(todo_status_icon("pending"), "⬜");
        assert_eq!(todo_status_icon("unknown"), "⬜");
        assert_eq!(todo_status_icon("cancelled"), "⬜");
        assert_eq!(todo_status_icon(""), "⬜");
    }

    #[test]
    fn test_todo_embed_color_all_completed() {
        let todos = [
            serde_json::json!({"id": "1", "content": "a", "status": "completed"}),
            serde_json::json!({"id": "2", "content": "b", "status": "completed"}),
        ];
        let refs: Vec<&serde_json::Value> = todos.iter().collect();
        assert_eq!(todo_embed_color(&refs), 0x2ECC71u32, "all completed → green");
    }

    #[test]
    fn test_todo_embed_color_has_in_progress() {
        let todos = [
            serde_json::json!({"id": "1", "content": "a", "status": "completed"}),
            serde_json::json!({"id": "2", "content": "b", "status": "in_progress"}),
        ];
        let refs: Vec<&serde_json::Value> = todos.iter().collect();
        assert_eq!(todo_embed_color(&refs), 0x3498DBu32, "has in_progress → blue");
    }

    #[test]
    fn test_todo_embed_color_all_pending() {
        let todos = [
            serde_json::json!({"id": "1", "content": "a", "status": "pending"}),
            serde_json::json!({"id": "2", "content": "b", "status": "pending"}),
        ];
        let refs: Vec<&serde_json::Value> = todos.iter().collect();
        assert_eq!(todo_embed_color(&refs), 0x95A5A6u32, "all pending → gray");
    }

    #[test]
    fn test_format_todo_embed_basic() {
        let input = serde_json::json!({
            "todos": [
                {"id": "1", "content": "Fix bug", "status": "completed"},
                {"id": "2", "content": "Write tests", "status": "in_progress"},
                {"id": "3", "content": "Deploy", "status": "pending"}
            ]
        });
        let embed = format_todo_embed(&input).expect("should return Some for non-empty todos");
        let v = serde_json::to_value(&embed).expect("embed should serialize");

        let title = v["title"].as_str().unwrap_or("");
        assert_eq!(title, "Tasks · 1/3", "title should show 1/3 completed");

        let desc = v["description"].as_str().unwrap_or("");
        assert!(desc.contains("✅"), "description should contain ✅ for completed");
        assert!(desc.contains("🔄"), "description should contain 🔄 for in_progress");
        assert!(desc.contains("⬜"), "description should contain ⬜ for pending");
    }

    #[test]
    fn test_format_todo_embed_all_completed() {
        let input = serde_json::json!({
            "todos": [
                {"id": "1", "content": "a", "status": "completed"},
                {"id": "2", "content": "b", "status": "completed"},
            ]
        });
        let embed = format_todo_embed(&input).expect("should return Some");
        let v = serde_json::to_value(&embed).expect("embed should serialize");
        let color = v["color"].as_u64().unwrap_or(0);
        assert_eq!(color, 0x2ECC71u64, "all completed → green color");
    }

    #[test]
    fn test_format_todo_embed_empty_todos() {
        let input = serde_json::json!({"todos": []});
        assert!(format_todo_embed(&input).is_none(), "empty todos array → None");
    }

    #[test]
    fn test_format_todo_embed_missing_todos_field() {
        let input = serde_json::json!({"other": "value"});
        assert!(format_todo_embed(&input).is_none(), "missing todos field → None");
    }

    #[test]
    fn test_format_todo_embed_folding() {
        // 12 completed + 3 pending = 15 items total
        let mut todos = vec![];
        for i in 0..12 {
            todos.push(serde_json::json!({"id": i.to_string(), "content": format!("done-{}", i), "status": "completed"}));
        }
        for i in 0..3 {
            todos.push(serde_json::json!({"id": format!("p{}", i), "content": format!("todo-{}", i), "status": "pending"}));
        }
        let input = serde_json::json!({"todos": todos});
        let embed = format_todo_embed(&input).expect("should return Some");
        let v = serde_json::to_value(&embed).expect("serialize");
        let desc = v["description"].as_str().unwrap_or("");
        assert!(
            desc.contains("+9 more completed"),
            "should fold 9 completed items into '+9 more completed', got: {}",
            desc
        );
    }

    #[test]
    fn test_format_todo_embed_unknown_status() {
        let input = serde_json::json!({
            "todos": [
                {"id": "1", "content": "Cancelled task", "status": "cancelled"}
            ]
        });
        let embed = format_todo_embed(&input).expect("should return Some");
        let v = serde_json::to_value(&embed).expect("serialize");
        let desc = v["description"].as_str().unwrap_or("");
        assert!(desc.contains("⬜"), "unknown status → ⬜ icon");
        assert!(desc.contains("Cancelled task"), "content should appear in description");
    }

    #[test]
    fn test_format_todo_embed_ordering() {
        // Items delivered in mixed order; output must be: in_progress → pending → completed
        let input = serde_json::json!({
            "todos": [
                {"id": "1", "content": "Pending item", "status": "pending"},
                {"id": "2", "content": "Completed item", "status": "completed"},
                {"id": "3", "content": "InProgress item", "status": "in_progress"},
            ]
        });
        let embed = format_todo_embed(&input).expect("should return Some");
        let v = serde_json::to_value(&embed).expect("serialize");
        let desc = v["description"].as_str().unwrap_or("");

        let pos_in_progress = desc.find("InProgress item").expect("InProgress item must appear");
        let pos_pending = desc.find("Pending item").expect("Pending item must appear");
        let pos_completed = desc.find("Completed item").expect("Completed item must appear");

        assert!(
            pos_in_progress < pos_pending,
            "in_progress should come before pending"
        );
        assert!(
            pos_pending < pos_completed,
            "pending should come before completed"
        );
    }

    // --- convert_html_details tests ---

    #[test]
    fn convert_details_basic() {
        let input = "<details>\n<summary>제목</summary>\n내용\n</details>";
        let result = convert_html_details(input);
        assert_eq!(result, "**제목**\n> 내용");
    }

    #[test]
    fn convert_details_without_summary() {
        let input = "<details>\n내용만\n</details>";
        let result = convert_html_details(input);
        assert_eq!(result, "> 내용만");
    }

    #[test]
    fn convert_details_in_code_block() {
        let input = "```\n<details>\n<summary>코드 예시</summary>\n</details>\n```";
        let result = convert_html_details(input);
        assert_eq!(result, input);
    }

    #[test]
    fn convert_details_compact_single_line() {
        let input = "<details><summary>Title</summary>Content</details>";
        let result = convert_html_details(input);
        assert_eq!(result, "**Title**\n> Content");
    }

    #[test]
    fn convert_details_malformed_no_closing() {
        let input = "<details>\n<summary>열림</summary>\n내용";
        let result = convert_html_details(input);
        assert_eq!(result, input);
    }

    #[test]
    fn convert_details_with_attributes() {
        let input = "<details open>\n<summary>펼침</summary>\n내용\n</details>";
        let result = convert_html_details(input);
        assert_eq!(result, "**펼침**\n> 내용");
    }

    #[test]
    fn convert_details_empty_lines_in_content() {
        let input = "<details>\n<summary>제목</summary>\n첫째\n\n둘째\n</details>";
        let result = convert_html_details(input);
        assert_eq!(result, "**제목**\n> 첫째\n>\n> 둘째");
    }

    #[test]
    fn convert_details_consecutive() {
        let input = "<details>\n<summary>A</summary>\n1\n</details>\n<details>\n<summary>B</summary>\n2\n</details>";
        let result = convert_html_details(input);
        assert_eq!(result, "**A**\n> 1\n**B**\n> 2");
    }

    #[test]
    fn convert_details_preserves_surrounding_text() {
        let input = "앞 텍스트\n<details>\n<summary>제목</summary>\n내용\n</details>\n뒤 텍스트";
        let result = convert_html_details(input);
        assert_eq!(result, "앞 텍스트\n**제목**\n> 내용\n뒤 텍스트");
    }

    #[test]
    fn convert_details_nested_flat() {
        let input = "<details>\n<summary>외부</summary>\n<details>\n<summary>내부</summary>\n내용\n</details>\n</details>";
        let result = convert_html_details(input);
        assert_eq!(result, "**내부**\n> <details>\n> 내용\n</details>");
    }

    #[test]
    fn convert_details_no_details_passthrough() {
        let input = "일반 텍스트\n코드 없음";
        let result = convert_html_details(input);
        assert_eq!(result, input);
    }

    #[test]
    fn convert_details_prefix_text_preserved() {
        let input = "before text <details>\n<summary>T</summary>\ncontent\n</details>";
        let result = convert_html_details(input);
        assert_eq!(result, "before text\n**T**\n> content");
    }

    #[test]
    fn convert_details_suffix_text_preserved() {
        let input = "<details>\n<summary>T</summary>\ncontent\n</details> after text";
        let result = convert_html_details(input);
        assert_eq!(result, "**T**\n> content\nafter text");
    }

    #[test]
    fn convert_details_compact_prefix_suffix_preserved() {
        let input = "before <details><summary>T</summary>content</details> after";
        let result = convert_html_details(input);
        assert_eq!(result, "before\n**T**\n> content\nafter");
    }
}
