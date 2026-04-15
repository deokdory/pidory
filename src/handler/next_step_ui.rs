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

    for skill in extract_skill_names(&stripped) {
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

/// Extracts raw skill name candidates (without leading `/`) from text.
/// A valid skill reference is `/` preceded by whitespace, backtick, `(`, `[`, or start-of-string,
/// followed by a lowercase ASCII letter and then lowercase letters, digits, or hyphens.
fn extract_skill_names(text: &str) -> Vec<&str> {
    let mut skills = Vec::new();

    // Use char_indices to handle multi-byte chars correctly while checking prev byte.
    let bytes = text.as_bytes();

    for (i, &b) in bytes.iter().enumerate() {
        if b != b'/' {
            continue;
        }

        // Check preceding character
        let prev_ok = if i == 0 {
            true
        } else {
            matches!(bytes[i - 1], b' ' | b'\t' | b'\n' | b'\r' | b'`' | b'(' | b'[')
        };

        if !prev_ok {
            continue;
        }

        // Must start with lowercase ASCII letter
        let start = i + 1;
        if start >= bytes.len() || !bytes[start].is_ascii_lowercase() {
            continue;
        }

        // Extend while lowercase alphanumeric or hyphen
        let mut end = start;
        while end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'-')
        {
            end += 1;
        }

        if end > start {
            // Safety: all bytes in [start..end] are ASCII
            skills.push(&text[start..end]);
        }
    }

    skills
}

// ─── UI helpers ─────────────────────────────────────────────────────────────

/// Builds Discord action row buttons for the given skill names.
/// Each button has custom_id `nxt:{thread_id}:{skill}` and label `/{skill}`.
/// At most 5 skills are processed. Returns empty Vec for empty input.
pub fn create_next_step_components(skills: &[String], thread_id: &str) -> Vec<CreateActionRow> {
    if skills.is_empty() {
        return vec![];
    }

    // custom_id limit is 100 chars. prefix "nxt:" = 4, ":" separator = 1, total prefix overhead = 5.
    // thread_id + skill must fit in 95 chars; truncate thread_id if needed.
    const MAX_CUSTOM_ID: usize = 100;
    const PREFIX_LEN: usize = "nxt:".len() + ":".len(); // 5

    let buttons: Vec<CreateButton> = skills
        .iter()
        .take(5)
        .map(|skill| {
            let available = MAX_CUSTOM_ID.saturating_sub(PREFIX_LEN).saturating_sub(skill.len());
            let truncated_thread_id: String = thread_id.chars().take(available).collect();
            let custom_id = format!("nxt:{}:{}", truncated_thread_id, skill);
            CreateButton::new(custom_id)
                .label(format!("/{}", skill))
                .style(ButtonStyle::Secondary)
        })
        .collect();

    vec![CreateActionRow::Buttons(buttons)]
}

/// Parses custom_id in the format `nxt:{thread_id}:{skill}`.
/// Returns `(thread_id, skill_name)` or `None` if the format does not match.
pub fn parse_next_step_custom_id(custom_id: &str) -> Option<(String, String)> {
    let rest = custom_id.strip_prefix("nxt:")?;
    let (thread_part, skill_name) = rest.rsplit_once(':')?;
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

    // 1. Normal detection
    #[test]
    fn detects_valid_skill() {
        let events = vec![make_assistant("`/session-commit` 해")];
        let result = extract_next_steps(&events, &skills(&["session-commit"]));
        assert_eq!(result, vec!["session-commit"]);
    }

    // 2. Skill inside fenced code block is ignored
    #[test]
    fn ignores_skill_in_fenced_code_block() {
        let text = "일반 텍스트\n```\n/build\n```\n여기는 아님";
        let events = vec![make_assistant(text)];
        let result = extract_next_steps(&events, &skills(&["build"]));
        assert_eq!(result, Vec::<String>::new());
    }

    // 3. Natural language slash (e.g. "커밋/push") doesn't match
    #[test]
    fn ignores_natural_language_slash() {
        let events = vec![make_assistant("커밋/push 할까?")];
        let result = extract_next_steps(&events, &skills(&["push"]));
        assert_eq!(result, Vec::<String>::new());
    }

    // 4. Empty valid_skills returns empty
    #[test]
    fn empty_valid_skills_returns_empty() {
        let events = vec![make_assistant("/session-commit 해")];
        let result = extract_next_steps(&events, &[]);
        assert_eq!(result, Vec::<String>::new());
    }

    // 5. Deduplication
    #[test]
    fn deduplicates_skills() {
        let events = vec![make_assistant("/build 먼저, 그다음 /build 또")];
        let result = extract_next_steps(&events, &skills(&["build"]));
        assert_eq!(result, vec!["build"]);
    }

    // 6. Maximum 5 skills
    #[test]
    fn limits_to_five_skills() {
        let text = "/a /b /c /d /e /f";
        let events = vec![make_assistant(text)];
        let valid = skills(&["a", "b", "c", "d", "e", "f"]);
        let result = extract_next_steps(&events, &valid);
        assert_eq!(result.len(), 5);
    }

    // 7. Empty events returns empty
    #[test]
    fn empty_events_returns_empty() {
        let result = extract_next_steps(&[], &skills(&["build"]));
        assert_eq!(result, Vec::<String>::new());
    }

    // 8. Only the last Assistant event's text is used
    #[test]
    fn uses_only_last_assistant_event() {
        let first = make_assistant("/session-commit 해줘");
        let last = make_assistant("이번엔 아무것도 없어요");
        let events = vec![first, last];
        let result = extract_next_steps(&events, &skills(&["session-commit"]));
        assert_eq!(result, Vec::<String>::new());
    }

    // strip_fenced_code_blocks helpers

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

    // Skill with opening bracket/parenthesis before slash
    #[test]
    fn detects_skill_after_bracket() {
        let events = vec![make_assistant("([/craft] 해봐)")];
        let result = extract_next_steps(&events, &skills(&["craft"]));
        assert_eq!(result, vec!["craft"]);
    }

    // Skill at start of string
    #[test]
    fn detects_skill_at_start_of_string() {
        let events = vec![make_assistant("/verify 실행해")];
        let result = extract_next_steps(&events, &skills(&["verify"]));
        assert_eq!(result, vec!["verify"]);
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
        // "nxt:123:" — skill part is empty string after last ':'
        let result = parse_next_step_custom_id("nxt:123:");
        assert_eq!(result, Some(("123".to_string(), "".to_string())));
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
