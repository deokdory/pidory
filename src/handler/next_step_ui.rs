use std::collections::HashSet;

use poise::serenity_prelude::{
    ButtonStyle, ChannelId, Context, CreateActionRow, CreateButton, EditMessage, MessageId,
};

use crate::subprocess::parser::{ContentBlock, StreamEvent};

/// Removes fenced code blocks (``` ... ```) from text.
/// Inline code (single backtick) is preserved.
pub(crate) fn strip_fenced_code_blocks(text: &str) -> String {
    let mut result = String::new();
    let mut remaining = text;

    while let Some(fence_start) = find_fence(remaining) {
        // Keep everything before the fence
        result.push_str(&remaining[..fence_start]);

        // Find the closing fence after the opening
        let after_open = &remaining[fence_start + 3..];
        // Skip to end of the opening fence line
        let newline_pos = after_open.find('\n').unwrap_or(after_open.len());
        let after_open_line = &after_open[newline_pos..];

        if let Some(close_pos) = find_fence(after_open_line) {
            // Skip past the closing ``` (find end of closing fence line)
            let after_close = &after_open_line[close_pos + 3..];
            let end_of_close = after_close
                .find('\n')
                .map(|p| p + 1)
                .unwrap_or(after_close.len());
            remaining = &after_open_line[close_pos + 3 + end_of_close..];
        } else {
            // No closing fence — drop the rest
            remaining = "";
        }
    }

    result.push_str(remaining);
    result
}

/// Finds the byte offset of the next ``` that starts at a line boundary
/// (position 0 or preceded by a newline).
fn find_fence(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i + 2 < len {
        if bytes[i] == b'`' && bytes[i + 1] == b'`' && bytes[i + 2] == b'`' {
            // Must be at start of text or right after a newline
            if i == 0 || bytes[i - 1] == b'\n' {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Extracts skill names referenced in the last Assistant event's last text block.
/// Only skills present in `valid_skills` are returned.
/// Returns at most 5 unique skill names.
pub fn extract_next_steps(events: &[StreamEvent], valid_skills: &[String]) -> Vec<String> {
    if valid_skills.is_empty() {
        return vec![];
    }

    // Find last Assistant event
    let last_assistant = events
        .iter()
        .rev()
        .find_map(|e| {
            if let StreamEvent::Assistant { content, .. } = e {
                Some(content)
            } else {
                None
            }
        });

    let content: &Vec<ContentBlock> = match last_assistant {
        Some(c) => c,
        None => return vec![],
    };

    // Get last Text block
    let last_text = content.iter().rev().find_map(|block| {
        if let ContentBlock::Text(t) = block {
            Some(t.as_str())
        } else {
            None
        }
    });

    let text = match last_text {
        Some(t) => t,
        None => return vec![],
    };

    let stripped = strip_fenced_code_blocks(text);
    let valid_set: HashSet<&str> = valid_skills.iter().map(|s| s.as_str()).collect();

    let mut seen: HashSet<String> = HashSet::new();
    let mut result: Vec<String> = Vec::new();

    for skill in parse_next_step_marker(&stripped) {
        if result.len() >= 5 {
            break;
        }
        if valid_set.contains(skill) && !seen.contains(skill) {
            seen.insert(skill.to_string());
            result.push(skill.to_string());
        }
    }

    result
}

/// Parses `<!-- next: skill1, skill2 -->` markers from text.
/// Returns skill names without leading `/`.
fn parse_next_step_marker(text: &str) -> Vec<&str> {
    let mut skills = Vec::new();
    let mut pos = 0;

    while pos < text.len() {
        let remaining = &text[pos..];
        let Some(open) = remaining.find("<!--") else {
            break;
        };
        let after_open = &remaining[open + 4..];
        let Some(close) = after_open.find("-->") else {
            break;
        };
        let inner = after_open[..close].trim();
        if let Some(list) = inner.strip_prefix("next:") {
            for item in list.split(',') {
                let skill = item.trim().trim_start_matches('/');
                if !skill.is_empty() {
                    skills.push(skill);
                }
            }
        }
        pos += open + 4 + close + 3;
    }

    skills
}

/// Strips `<!-- next: ... -->` markers from text for display.
pub fn strip_next_step_markers(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut pos = 0;

    while pos < text.len() {
        let remaining = &text[pos..];
        let Some(open) = remaining.find("<!--") else {
            result.push_str(remaining);
            break;
        };
        let after_open = &remaining[open + 4..];
        let Some(close) = after_open.find("-->") else {
            result.push_str(remaining);
            break;
        };
        let inner = after_open[..close].trim();
        if inner.starts_with("next:") {
            result.push_str(&remaining[..open]);
            pos += open + 4 + close + 3;
        } else {
            result.push_str(&remaining[..open + 4 + close + 3]);
            pos += open + 4 + close + 3;
        }
    }

    let len = result.trim_end().len();
    result.truncate(len);
    result
}

// ─── UI helpers ─────────────────────────────────────────────────────────────

/// Builds Discord action row buttons for the given skill names.
/// Each button has custom_id `nxt:{thread_id}:{skill}` and label `/{skill}`.
/// At most 5 skills are processed. Returns empty Vec for empty input.
pub fn create_next_step_components(skills: &[String], thread_id: &str) -> Vec<CreateActionRow> {
    if skills.is_empty() {
        return vec![];
    }

    const MAX_CUSTOM_ID: usize = 100;

    let buttons: Vec<CreateButton> = skills
        .iter()
        .take(5)
        .filter_map(|skill| {
            let custom_id = format!("nxt:{}:{}", thread_id, skill);
            if custom_id.len() > MAX_CUSTOM_ID {
                return None;
            }
            Some(
                CreateButton::new(custom_id)
                    .label(format!("/{}", skill))
                    .style(ButtonStyle::Secondary),
            )
        })
        .collect();

    vec![CreateActionRow::Buttons(buttons)]
}

/// Parses custom_id in the format `nxt:{thread_id}:{skill}`.
/// Returns `(thread_id, skill_name)` or `None` if the format does not match.
pub fn parse_next_step_custom_id(custom_id: &str) -> Option<(String, String)> {
    let rest = custom_id.strip_prefix("nxt:")?;
    let (thread_part, skill_name) = rest.rsplit_once(':')?;
    if skill_name.is_empty() {
        return None;
    }
    Some((thread_part.to_string(), skill_name.to_string()))
}

/// Removes buttons from an existing Discord message by replacing components with an empty list.
pub async fn disable_next_step_buttons(
    ctx: &Context,
    channel_id: ChannelId,
    message_id: MessageId,
) -> Result<(), serenity::Error> {
    channel_id
        .edit_message(ctx, message_id, EditMessage::new().components(vec![]))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subprocess::parser::StreamEvent;

    fn make_assistant(text: &str) -> StreamEvent {
        StreamEvent::Assistant {
            content: vec![ContentBlock::Text(text.to_string())],
            session_id: "test-session".to_string(),
        }
    }

    fn skills(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // ─── parse_next_step_marker ─────────────────────────────────────────────

    #[test]
    fn marker_single_skill() {
        let result = parse_next_step_marker("<!-- next: session-commit -->");
        assert_eq!(result, vec!["session-commit"]);
    }

    #[test]
    fn marker_multiple_skills() {
        let result = parse_next_step_marker("<!-- next: session-commit, my-pr, verify -->");
        assert_eq!(result, vec!["session-commit", "my-pr", "verify"]);
    }

    #[test]
    fn marker_with_slash_prefix() {
        let result = parse_next_step_marker("<!-- next: /session-commit, /my-pr -->");
        assert_eq!(result, vec!["session-commit", "my-pr"]);
    }

    #[test]
    fn marker_empty() {
        let result = parse_next_step_marker("<!-- next: -->");
        assert!(result.is_empty());
    }

    #[test]
    fn marker_no_spaces() {
        let result = parse_next_step_marker("<!--next:session-commit-->");
        assert_eq!(result, vec!["session-commit"]);
    }

    #[test]
    fn marker_extra_spaces() {
        let result = parse_next_step_marker("<!--  next:  session-commit ,  my-pr  -->");
        assert_eq!(result, vec!["session-commit", "my-pr"]);
    }

    #[test]
    fn marker_empty_entries() {
        let result = parse_next_step_marker("<!-- next: skill1,,skill2 -->");
        assert_eq!(result, vec!["skill1", "skill2"]);
    }

    #[test]
    fn no_marker_returns_empty() {
        let result = parse_next_step_marker("일반 텍스트 /session-commit");
        assert!(result.is_empty());
    }

    #[test]
    fn multiple_markers_combined() {
        let text = "part1\n<!-- next: build -->\npart2\n<!-- next: verify -->";
        let result = parse_next_step_marker(text);
        assert_eq!(result, vec!["build", "verify"]);
    }

    #[test]
    fn non_next_html_comment_ignored() {
        let result = parse_next_step_marker("<!-- some other comment -->");
        assert!(result.is_empty());
    }

    // ─── extract_next_steps (marker-based) ──────────────────────────────────

    #[test]
    fn detects_marker_skill() {
        let events = vec![make_assistant("작업 완료\n<!-- next: session-commit -->")];
        let result = extract_next_steps(&events, &skills(&["session-commit"]));
        assert_eq!(result, vec!["session-commit"]);
    }

    #[test]
    fn ignores_plain_slash_skill() {
        let events = vec![make_assistant("`/session-commit` 해")];
        let result = extract_next_steps(&events, &skills(&["session-commit"]));
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn ignores_marker_in_code_block() {
        let text = "일반 텍스트\n```\n<!-- next: build -->\n```\n여기는 아님";
        let events = vec![make_assistant(text)];
        let result = extract_next_steps(&events, &skills(&["build"]));
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn filters_invalid_skills() {
        let events = vec![make_assistant("<!-- next: build, nonexistent -->")];
        let result = extract_next_steps(&events, &skills(&["build"]));
        assert_eq!(result, vec!["build"]);
    }

    #[test]
    fn deduplicates_marker_skills() {
        let events = vec![make_assistant("<!-- next: build, build -->")];
        let result = extract_next_steps(&events, &skills(&["build"]));
        assert_eq!(result, vec!["build"]);
    }

    #[test]
    fn limits_to_five() {
        let events = vec![make_assistant("<!-- next: a, b, c, d, e, f -->")];
        let valid = skills(&["a", "b", "c", "d", "e", "f"]);
        let result = extract_next_steps(&events, &valid);
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn empty_events_returns_empty() {
        let result = extract_next_steps(&[], &skills(&["build"]));
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn empty_valid_skills_returns_empty() {
        let events = vec![make_assistant("<!-- next: session-commit -->")];
        let result = extract_next_steps(&events, &[]);
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn uses_only_last_assistant_event() {
        let first = make_assistant("<!-- next: session-commit -->");
        let last = make_assistant("이번엔 아무것도 없어요");
        let events = vec![first, last];
        let result = extract_next_steps(&events, &skills(&["session-commit"]));
        assert_eq!(result, Vec::<String>::new());
    }

    // ─── strip_fenced_code_blocks ───────────────────────────────────────────

    #[test]
    fn strip_removes_fenced_block() {
        let text = "before\n```\n/build\n```\nafter";
        let result = strip_fenced_code_blocks(text);
        assert!(!result.contains("/build"));
        assert!(result.contains("before"));
        assert!(result.contains("after"));
    }

    #[test]
    fn strip_preserves_inline_code() {
        let text = "run `cargo build` now";
        let result = strip_fenced_code_blocks(text);
        assert_eq!(result, text);
    }

    #[test]
    fn strip_language_hint_fenced_block() {
        let text = "```rust\nfn main() {}\n```\nend";
        let result = strip_fenced_code_blocks(text);
        assert!(!result.contains("fn main"));
        assert!(result.contains("end"));
    }

    // ─── strip_next_step_markers ────────────────────────────────────────────

    #[test]
    fn strips_marker_from_text() {
        let result = strip_next_step_markers("작업 완료\n<!-- next: session-commit -->");
        assert_eq!(result, "작업 완료");
    }

    #[test]
    fn strips_marker_trailing_newline() {
        let result = strip_next_step_markers("작업 완료\n<!-- next: session-commit -->\n");
        assert_eq!(result, "작업 완료");
    }

    #[test]
    fn no_marker_unchanged() {
        let text = "일반 텍스트입니다";
        let result = strip_next_step_markers(text);
        assert_eq!(result, text);
    }

    #[test]
    fn multiple_markers_stripped() {
        let result = strip_next_step_markers("text<!-- next: a -->middle<!-- next: b -->end");
        assert_eq!(result, "textmiddleend");
    }

    #[test]
    fn preserves_non_next_html_comment() {
        let result = strip_next_step_markers("text<!-- some comment -->end");
        assert_eq!(result, "text<!-- some comment -->end");
    }

    // ─── parse_next_step_custom_id ───────────────────────────────────────────

    #[test]
    fn parse_next_step_valid() {
        let result = parse_next_step_custom_id("nxt:1234567890:session-commit");
        assert_eq!(
            result,
            Some(("1234567890".to_string(), "session-commit".to_string()))
        );
    }

    #[test]
    fn parse_next_step_wrong_prefix() {
        let result = parse_next_step_custom_id("perm:123:allow");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_next_step_no_second_separator() {
        // Only one colon after "nxt:" — rsplit_once(':') still finds it, but there's no thread_id
        // "nxt:no-separator" strips to "no-separator", rsplit_once finds no ':', returns None
        let result = parse_next_step_custom_id("nxt:no-separator");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_next_step_empty_skill() {
        let result = parse_next_step_custom_id("nxt:123:");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_next_step_multiple_colons_uses_last() {
        // thread_id may contain colons (e.g. IPv6-like), rsplit_once splits on the last ':'
        let result = parse_next_step_custom_id("nxt:abc:def:craft");
        assert_eq!(
            result,
            Some(("abc:def".to_string(), "craft".to_string()))
        );
    }

    // ─── create_next_step_components ────────────────────────────────────────

    #[test]
    fn create_components_empty_input_returns_empty() {
        let result = create_next_step_components(&[], "123456");
        assert!(result.is_empty());
    }

    #[test]
    fn create_components_two_skills_one_action_row() {
        let skill_names = skills(&["build", "verify"]);
        let result = create_next_step_components(&skill_names, "987654321");
        assert_eq!(result.len(), 1);
        if let CreateActionRow::Buttons(buttons) = &result[0] {
            assert_eq!(buttons.len(), 2);
        } else {
            panic!("expected Buttons action row");
        }
    }

    #[test]
    fn create_components_six_skills_capped_at_five() {
        let skill_names = skills(&["a", "b", "c", "d", "e", "f"]);
        let result = create_next_step_components(&skill_names, "111");
        assert_eq!(result.len(), 1);
        if let CreateActionRow::Buttons(buttons) = &result[0] {
            assert_eq!(buttons.len(), 5);
        } else {
            panic!("expected Buttons action row");
        }
    }

    #[test]
    fn create_components_single_skill_correct_custom_id() {
        let skill_names = skills(&["session-commit"]);
        let result = create_next_step_components(&skill_names, "42");
        assert_eq!(result.len(), 1);
        if let CreateActionRow::Buttons(buttons) = &result[0] {
            assert_eq!(buttons.len(), 1);
            // Verify the round-trip: parse what we created
            let custom_id_str = format!("nxt:{}:{}", "42", "session-commit");
            let parsed = parse_next_step_custom_id(&custom_id_str);
            assert_eq!(
                parsed,
                Some(("42".to_string(), "session-commit".to_string()))
            );
        } else {
            panic!("expected Buttons action row");
        }
    }
}
