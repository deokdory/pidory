use poise::serenity_prelude::{
    ButtonStyle, ChannelId, Context, CreateActionRow, CreateButton, CreateInputText,
    CreateInteractionResponseMessage, CreateMessage, CreateModal, CreateSelectMenu,
    CreateSelectMenuKind, CreateSelectMenuOption, EditMessage, InputTextStyle, MessageId, UserId,
};
use serde_json::Value;

use crate::error::PidoryError;
use crate::i18n::Lang;

// ─── Sub-request-id helpers ─────────────────────────────────────────────────

const SUB_ID_SEPARATOR: &str = "__q";

/// Builds a sub-request-id: `{request_id}__q{question_index}`.
pub fn make_sub_request_id(request_id: &str, question_index: usize) -> String {
    format!("{}{}{}", request_id, SUB_ID_SEPARATOR, question_index)
}

/// Parses `{group_id}__q{index}` → `(group_id, index)`.
pub fn parse_sub_request_id(sub_id: &str) -> Option<(String, usize)> {
    let (group_id, idx_str) = sub_id.rsplit_once(SUB_ID_SEPARATOR)?;
    let idx: usize = idx_str.parse().ok()?;
    Some((group_id.to_string(), idx))
}

/// Returns the number of questions in the input.
pub fn question_count(input: &Value) -> usize {
    input
        .get("questions")
        .and_then(|q| q.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0)
}

// ─── Question message creation ──────────────────────────────────────────────

/// Creates a Discord message for a single question (first question in the input).
/// For multi-question inputs, use `create_question_message_for_index` instead.
pub fn create_question_message(
    input: &Value,
    request_id: &str,
    triggered_by: UserId,
    lang: Lang,
) -> CreateMessage {
    create_question_message_for_index(input, 0, request_id, triggered_by, lang)
}

/// Creates a Discord message for a specific question by index.
/// The `request_id` used in custom_ids is the sub-request-id (already includes `__q{idx}`).
///
/// - 2-5 options → Buttons (+ free text button)
/// - 6-25 options → Select Menu (+ free text button)
/// - No options → Free text button only
/// + Cancel button (with ephemeral confirm)
pub fn create_question_message_for_index(
    input: &Value,
    question_index: usize,
    sub_request_id: &str,
    triggered_by: UserId,
    lang: Lang,
) -> CreateMessage {
    let question = extract_question_at(input, question_index);
    let q_text = question
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let header = question
        .get("header")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let options: Vec<(String, String)> = extract_options(&question);

    let header_text = if header.is_empty() {
        String::new()
    } else {
        format!(" — **{}**", header)
    };

    let content = format!("<@{}>{}\n{}", triggered_by, header_text, q_text);

    let mut components = Vec::new();
    let rid = truncate_request_id(sub_request_id);

    if options.len() >= 6 {
        let capped_options = if options.len() > 25 {
            tracing::warn!(
                "AskUserQuestion q{} has {} options, capping to 25 for Discord select menu",
                question_index,
                options.len()
            );
            &options[..25]
        } else {
            &options
        };
        let menu_options: Vec<CreateSelectMenuOption> = capped_options
            .iter()
            .enumerate()
            .map(|(i, (label, desc))| {
                let mut opt = CreateSelectMenuOption::new(label, i.to_string());
                if !desc.is_empty() {
                    opt = opt.description(desc);
                }
                opt
            })
            .collect();
        let select = CreateSelectMenu::new(
            format!("ask_sel:{}", rid),
            CreateSelectMenuKind::String {
                options: menu_options,
            },
        )
        .placeholder(lang.question_select_placeholder());
        components.push(CreateActionRow::SelectMenu(select));
    } else if !options.is_empty() {
        let buttons: Vec<CreateButton> = options
            .iter()
            .enumerate()
            .map(|(i, (label, _desc))| {
                CreateButton::new(format!("ask:{}:{}", rid, i))
                    .label(label)
                    .style(ButtonStyle::Primary)
            })
            .collect();
        components.push(CreateActionRow::Buttons(buttons));
    }

    let text_button = CreateButton::new(format!("ask_text:{}", rid))
        .label(lang.question_write_answer())
        .style(ButtonStyle::Secondary);
    components.push(CreateActionRow::Buttons(vec![text_button]));

    let cancel_button = CreateButton::new(format!("ask_cancel:{}", rid))
        .label(lang.question_cancel())
        .style(ButtonStyle::Danger);
    components.push(CreateActionRow::Buttons(vec![cancel_button]));

    CreateMessage::new().content(content).components(components)
}

fn extract_question_at(input: &Value, index: usize) -> Value {
    input
        .get("questions")
        .and_then(|q| q.as_array())
        .and_then(|arr| arr.get(index))
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()))
}

fn extract_options(question: &Value) -> Vec<(String, String)> {
    question
        .get("options")
        .and_then(|o| o.as_array())
        .map(|arr| {
            arr.iter()
                .map(|opt| {
                    let label = opt
                        .get("label")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let desc = opt
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    (label, desc)
                })
                .collect()
        })
        .unwrap_or_default()
}

// ─── Modal ──────────────────────────────────────────────────────────────────

/// Creates a modal for free-text answer input.
pub fn create_question_modal(request_id: &str, lang: Lang) -> CreateModal {
    let input_field = CreateInputText::new(
        InputTextStyle::Paragraph,
        lang.question_modal_label(),
        "answer",
    )
    .placeholder(lang.question_modal_placeholder())
    .max_length(4000)
    .required(true);

    CreateModal::new(
        format!("ask_modal:{}", truncate_request_id(request_id)),
        lang.question_modal_title(),
    )
    .components(vec![CreateActionRow::InputText(input_field)])
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Truncates request_id to 80 chars, leaving room for prefixes and suffixes
/// within Discord's 100-char custom_id limit.
fn truncate_request_id(request_id: &str) -> &str {
    if request_id.len() > 80 {
        &request_id[..80]
    } else {
        request_id
    }
}

/// Parses `ask:{request_id}:{option_index}` button custom_id.
pub fn parse_question_button_id(custom_id: &str) -> Option<(String, usize)> {
    let stripped = custom_id.strip_prefix("ask:")?;
    let (request_id, index_str) = stripped.rsplit_once(':')?;
    let index: usize = index_str.parse().ok()?;
    Some((request_id.to_string(), index))
}

/// Parses `ask_text:{request_id}` free-text button custom_id.
pub fn parse_question_text_button_id(custom_id: &str) -> Option<String> {
    custom_id.strip_prefix("ask_text:").map(|s| s.to_string())
}

/// Parses `ask_sel:{request_id}` select menu custom_id.
pub fn parse_question_select_id(custom_id: &str) -> Option<String> {
    custom_id.strip_prefix("ask_sel:").map(|s| s.to_string())
}

/// Parses `ask_modal:{request_id}` modal custom_id.
pub fn parse_question_modal_id(custom_id: &str) -> Option<String> {
    custom_id.strip_prefix("ask_modal:").map(|s| s.to_string())
}

/// Parses `ask_cancel:{request_id}` cancel button custom_id.
///
/// Safe vs `ask_cancel_confirm:` / `ask_cancel_abort:` because those have `_` right
/// after `ask_cancel`, not `:` — `strip_prefix("ask_cancel:")` cannot match them.
pub fn parse_question_cancel_button_id(custom_id: &str) -> Option<String> {
    custom_id.strip_prefix("ask_cancel:").map(|s| s.to_string())
}

/// Parses `ask_cancel_confirm:{request_id}` confirm-yes button custom_id.
pub fn parse_question_cancel_confirm_id(custom_id: &str) -> Option<String> {
    custom_id
        .strip_prefix("ask_cancel_confirm:")
        .map(|s| s.to_string())
}

/// Parses `ask_cancel_abort:{request_id}` confirm-no button custom_id.
pub fn parse_question_cancel_abort_id(custom_id: &str) -> Option<String> {
    custom_id
        .strip_prefix("ask_cancel_abort:")
        .map(|s| s.to_string())
}

/// Resolves an option label from the original input, question index, and option index.
pub fn resolve_option_label(input: &Value, question_index: usize, option_index: usize) -> String {
    let question = extract_question_at(input, question_index);
    let options = extract_options(&question);
    options
        .get(option_index)
        .map(|(label, _)| label.clone())
        .unwrap_or_else(|| option_index.to_string())
}

/// Disables question components after an answer is selected.
pub async fn disable_question_components(
    ctx: &Context,
    channel_id: ChannelId,
    message_id: MessageId,
    answer: &str,
    lang: Lang,
) -> Result<(), PidoryError> {
    let label = format!("-# ✅ {} {}", lang.question_answered(), answer);
    disable_question_components_with_label(ctx, channel_id, message_id, &label).await
}

/// Disables question components with a caller-supplied label string.
/// Used by cancel flow to display `lang.question_canceled_label()`.
pub async fn disable_question_components_with_label(
    ctx: &Context,
    channel_id: ChannelId,
    message_id: MessageId,
    label: &str,
) -> Result<(), PidoryError> {
    let edit = EditMessage::new().content(label).components(vec![]);
    channel_id
        .edit_message(ctx, message_id, edit)
        .await
        .map_err(|e| PidoryError::Discord(Box::new(e)))?;
    Ok(())
}

/// Builds an ephemeral confirmation message for the cancel flow.
///
/// Contains two buttons in one row:
/// - `ask_cancel_confirm:{rid}` (Danger) — confirm cancel
/// - `ask_cancel_abort:{rid}` (Secondary) — go back
pub fn create_cancel_confirm_message(
    sub_request_id: &str,
    lang: Lang,
) -> CreateInteractionResponseMessage {
    let rid = truncate_request_id(sub_request_id);
    let confirm_btn = CreateButton::new(format!("ask_cancel_confirm:{}", rid))
        .label(lang.question_cancel_confirm_yes())
        .style(ButtonStyle::Danger);
    let abort_btn = CreateButton::new(format!("ask_cancel_abort:{}", rid))
        .label(lang.question_cancel_confirm_no())
        .style(ButtonStyle::Secondary);
    let row = CreateActionRow::Buttons(vec![confirm_btn, abort_btn]);
    CreateInteractionResponseMessage::new()
        .content(lang.question_cancel_confirm_prompt())
        .components(vec![row])
        .ephemeral(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sub-request-id helpers ──────────────────────────────────────────────

    #[test]
    fn make_sub_request_id_format() {
        assert_eq!(make_sub_request_id("req-abc", 0), "req-abc__q0");
        assert_eq!(make_sub_request_id("req-abc", 3), "req-abc__q3");
    }

    #[test]
    fn parse_sub_request_id_valid() {
        let (group, idx) = parse_sub_request_id("req-abc__q1").unwrap();
        assert_eq!(group, "req-abc");
        assert_eq!(idx, 1);
    }

    #[test]
    fn parse_sub_request_id_roundtrip() {
        let sub = make_sub_request_id("uuid-123-456", 2);
        let (group, idx) = parse_sub_request_id(&sub).unwrap();
        assert_eq!(group, "uuid-123-456");
        assert_eq!(idx, 2);
    }

    #[test]
    fn parse_sub_request_id_invalid() {
        assert!(parse_sub_request_id("no-separator").is_none());
        assert!(parse_sub_request_id("req__qnotnum").is_none());
    }

    #[test]
    fn parse_sub_request_id_with_colons_in_group() {
        let (group, idx) = parse_sub_request_id("a:b:c__q0").unwrap();
        assert_eq!(group, "a:b:c");
        assert_eq!(idx, 0);
    }

    // ── question_count ──────────────────────────────────────────────────────

    #[test]
    fn question_count_empty() {
        assert_eq!(question_count(&serde_json::json!({})), 0);
    }

    #[test]
    fn question_count_single() {
        let input = serde_json::json!({"questions": [{"question": "q?"}]});
        assert_eq!(question_count(&input), 1);
    }

    #[test]
    fn question_count_multiple() {
        let input = serde_json::json!({"questions": [{"question": "a?"}, {"question": "b?"}]});
        assert_eq!(question_count(&input), 2);
    }

    // ── parsing ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_question_button_valid() {
        let (rid, idx) = parse_question_button_id("ask:abc-123:2").unwrap();
        assert_eq!(rid, "abc-123");
        assert_eq!(idx, 2);
    }

    #[test]
    fn parse_question_button_with_sub_id() {
        let (rid, idx) = parse_question_button_id("ask:req-1__q0:3").unwrap();
        assert_eq!(rid, "req-1__q0");
        assert_eq!(idx, 3);
    }

    #[test]
    fn parse_question_button_invalid_prefix() {
        assert!(parse_question_button_id("perm:abc:0").is_none());
    }

    #[test]
    fn parse_question_button_invalid_index() {
        assert!(parse_question_button_id("ask:abc:notnum").is_none());
    }

    #[test]
    fn parse_question_text_button_valid() {
        let rid = parse_question_text_button_id("ask_text:req-123").unwrap();
        assert_eq!(rid, "req-123");
    }

    #[test]
    fn parse_question_text_button_with_sub_id() {
        let rid = parse_question_text_button_id("ask_text:req-123__q1").unwrap();
        assert_eq!(rid, "req-123__q1");
    }

    #[test]
    fn parse_question_text_button_invalid() {
        assert!(parse_question_text_button_id("ask:abc:0").is_none());
    }

    #[test]
    fn parse_question_select_valid() {
        let rid = parse_question_select_id("ask_sel:req-456").unwrap();
        assert_eq!(rid, "req-456");
    }

    #[test]
    fn parse_question_modal_valid() {
        let rid = parse_question_modal_id("ask_modal:req-789").unwrap();
        assert_eq!(rid, "req-789");
    }

    // ── resolve_option_label ────────────────────────────────────────────────

    #[test]
    fn resolve_option_label_valid() {
        let input = serde_json::json!({
            "questions": [{"question": "pick", "options": [
                {"label": "A", "description": "aa"},
                {"label": "B", "description": "bb"}
            ]}]
        });
        assert_eq!(resolve_option_label(&input, 0, 0), "A");
        assert_eq!(resolve_option_label(&input, 0, 1), "B");
    }

    #[test]
    fn resolve_option_label_second_question() {
        let input = serde_json::json!({
            "questions": [
                {"question": "q0", "options": [{"label": "X"}]},
                {"question": "q1", "options": [{"label": "Y"}, {"label": "Z"}]}
            ]
        });
        assert_eq!(resolve_option_label(&input, 1, 0), "Y");
        assert_eq!(resolve_option_label(&input, 1, 1), "Z");
    }

    #[test]
    fn resolve_option_label_out_of_bounds() {
        let input =
            serde_json::json!({"questions": [{"question": "q", "options": [{"label": "X"}]}]});
        assert_eq!(resolve_option_label(&input, 0, 5), "5");
    }

    // ── extract helpers ─────────────────────────────────────────────────────

    #[test]
    fn extract_question_at_empty() {
        let input = serde_json::json!({});
        let q = extract_question_at(&input, 0);
        assert!(q.is_object());
    }

    #[test]
    fn extract_question_at_second() {
        let input = serde_json::json!({"questions": [{"question": "a"}, {"question": "b"}]});
        let q = extract_question_at(&input, 1);
        assert_eq!(q["question"], "b");
    }

    #[test]
    fn extract_options_empty() {
        let q = serde_json::json!({"question": "hi"});
        let opts = extract_options(&q);
        assert!(opts.is_empty());
    }

    #[test]
    fn extract_options_with_items() {
        let q = serde_json::json!({"options": [{"label": "A", "description": "desc"}, {"label": "B"}]});
        let opts = extract_options(&q);
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].0, "A");
        assert_eq!(opts[0].1, "desc");
        assert_eq!(opts[1].0, "B");
        assert_eq!(opts[1].1, "");
    }

    // ── message creation ────────────────────────────────────────────────────

    #[test]
    fn create_question_message_for_each_index() {
        let input = serde_json::json!({
            "questions": [
                {"question": "First?", "header": "Q1", "options": [{"label": "A", "description": "a"}]},
                {"question": "Second?", "header": "Q2", "options": [{"label": "B", "description": "b"}]}
            ]
        });
        let msg0 = create_question_message_for_index(&input, 0, "req__q0", UserId::new(1), Lang::En);
        let json0 = serde_json::to_value(&msg0).unwrap();
        let content0 = json0["content"].as_str().unwrap_or("");
        assert!(content0.contains("First?"));
        assert!(!content0.contains("Second?"));

        let msg1 = create_question_message_for_index(&input, 1, "req__q1", UserId::new(1), Lang::En);
        let json1 = serde_json::to_value(&msg1).unwrap();
        let content1 = json1["content"].as_str().unwrap_or("");
        assert!(content1.contains("Second?"));
        assert!(!content1.contains("First?"));
    }

    #[test]
    fn create_question_message_caps_select_at_25() {
        let options: Vec<serde_json::Value> = (0..30)
            .map(|i| serde_json::json!({"label": format!("Opt {}", i)}))
            .collect();
        let input = serde_json::json!({"questions": [{"question": "pick", "options": options}]});
        // Should not panic — select menu capped at 25
        let _msg = create_question_message(&input, "req-cap", UserId::new(1), Lang::En);
    }

    #[test]
    fn truncate_request_id_short() {
        assert_eq!(truncate_request_id("abc-123"), "abc-123");
    }

    #[test]
    fn truncate_request_id_long() {
        let long = "a".repeat(120);
        assert_eq!(truncate_request_id(&long).len(), 80);
    }

    #[test]
    fn custom_ids_within_100_chars() {
        let long_id = "a".repeat(120);
        let rid = truncate_request_id(&long_id);
        assert!(format!("ask:{}:{}", rid, 99).len() <= 100);
        assert!(format!("ask_text:{}", rid).len() <= 100);
        assert!(format!("ask_sel:{}", rid).len() <= 100);
        assert!(format!("ask_modal:{}", rid).len() <= 100);
    }

    // ── ask_text button presence ────────────────────────────────────────────

    #[test]
    fn create_question_message_includes_text_button_with_buttons() {
        // 3 options → Buttons branch (2-5 options)
        let input = serde_json::json!({
            "questions": [{
                "question": "Pick one?",
                "options": [
                    {"label": "Alpha"},
                    {"label": "Beta"},
                    {"label": "Gamma"}
                ]
            }]
        });
        let msg = create_question_message_for_index(&input, 0, "req-btn", UserId::new(1), Lang::En);
        let json = serde_json::to_value(&msg).unwrap();
        let components = json["components"].as_array().cloned().unwrap_or_default();
        let count = components.iter().fold(0usize, |acc, row| {
            let items = row["components"].as_array().cloned().unwrap_or_default();
            acc + items.iter().filter(|item| {
                item["custom_id"]
                    .as_str()
                    .map(|cid| cid.starts_with("ask_text:"))
                    .unwrap_or(false)
            }).count()
        });
        assert_eq!(count, 1, "expected exactly 1 ask_text: button in components");
    }

    #[test]
    fn create_question_message_includes_text_button_with_select() {
        // 10 options → SelectMenu branch (6-25 options)
        let options: Vec<serde_json::Value> = (0..10)
            .map(|i| serde_json::json!({"label": format!("Option {}", i)}))
            .collect();
        let input = serde_json::json!({
            "questions": [{"question": "Choose?", "options": options}]
        });
        let msg =
            create_question_message_for_index(&input, 0, "req-sel", UserId::new(2), Lang::En);
        let json = serde_json::to_value(&msg).unwrap();
        let components = json["components"].as_array().cloned().unwrap_or_default();
        let count = components.iter().fold(0usize, |acc, row| {
            let items = row["components"].as_array().cloned().unwrap_or_default();
            acc + items.iter().filter(|item| {
                item["custom_id"]
                    .as_str()
                    .map(|cid| cid.starts_with("ask_text:"))
                    .unwrap_or(false)
            }).count()
        });
        assert_eq!(count, 1, "expected exactly 1 ask_text: button alongside select menu");
    }

    #[test]
    fn create_question_message_includes_text_button_with_no_options() {
        // 0 options → free text button only
        let input = serde_json::json!({
            "questions": [{"question": "Anything to say?"}]
        });
        let msg =
            create_question_message_for_index(&input, 0, "req-none", UserId::new(3), Lang::En);
        let json = serde_json::to_value(&msg).unwrap();
        let components = json["components"].as_array().cloned().unwrap_or_default();
        assert!(!components.is_empty(), "components must not be empty when no options given");
        let count = components.iter().fold(0usize, |acc, row| {
            let items = row["components"].as_array().cloned().unwrap_or_default();
            acc + items.iter().filter(|item| {
                item["custom_id"]
                    .as_str()
                    .map(|cid| cid.starts_with("ask_text:"))
                    .unwrap_or(false)
            }).count()
        });
        assert_eq!(count, 1, "expected exactly 1 ask_text: button when no options present");
    }

    // ── cancel button presence ──────────────────────────────────────────────

    #[test]
    fn create_question_message_includes_cancel_button_with_buttons() {
        // 3 options → Buttons branch; cancel button must also appear exactly once
        let input = serde_json::json!({
            "questions": [{
                "question": "Pick one?",
                "options": [
                    {"label": "Alpha"},
                    {"label": "Beta"},
                    {"label": "Gamma"}
                ]
            }]
        });
        let msg =
            create_question_message_for_index(&input, 0, "req-btn", UserId::new(1), Lang::En);
        let json = serde_json::to_value(&msg).unwrap();
        let components = json["components"].as_array().cloned().unwrap_or_default();
        let count = components.iter().fold(0usize, |acc, row| {
            let items = row["components"].as_array().cloned().unwrap_or_default();
            acc + items
                .iter()
                .filter(|item| {
                    item["custom_id"]
                        .as_str()
                        .map(|cid| cid.starts_with("ask_cancel:"))
                        .unwrap_or(false)
                })
                .count()
        });
        assert_eq!(count, 1, "expected exactly 1 ask_cancel: button");
    }

    #[test]
    fn create_question_message_includes_cancel_button_with_select() {
        // 10 options → SelectMenu branch; cancel button must appear alongside select menu
        let options: Vec<serde_json::Value> = (0..10)
            .map(|i| serde_json::json!({"label": format!("Option {}", i)}))
            .collect();
        let input = serde_json::json!({
            "questions": [{"question": "Choose?", "options": options}]
        });
        let msg =
            create_question_message_for_index(&input, 0, "req-sel", UserId::new(2), Lang::En);
        let json = serde_json::to_value(&msg).unwrap();
        let components = json["components"].as_array().cloned().unwrap_or_default();
        let count = components.iter().fold(0usize, |acc, row| {
            let items = row["components"].as_array().cloned().unwrap_or_default();
            acc + items
                .iter()
                .filter(|item| {
                    item["custom_id"]
                        .as_str()
                        .map(|cid| cid.starts_with("ask_cancel:"))
                        .unwrap_or(false)
                })
                .count()
        });
        assert_eq!(count, 1, "expected exactly 1 ask_cancel: button alongside select menu");
    }

    #[test]
    fn create_question_message_includes_cancel_button_with_no_options() {
        // 0 options → free text button + cancel button; at least 2 ActionRows
        let input = serde_json::json!({
            "questions": [{"question": "Anything to say?"}]
        });
        let msg =
            create_question_message_for_index(&input, 0, "req-none", UserId::new(3), Lang::En);
        let json = serde_json::to_value(&msg).unwrap();
        let components = json["components"].as_array().cloned().unwrap_or_default();
        assert!(
            components.len() >= 2,
            "expected at least 2 ActionRows (free text + cancel), got {}",
            components.len()
        );
        let count = components.iter().fold(0usize, |acc, row| {
            let items = row["components"].as_array().cloned().unwrap_or_default();
            acc + items
                .iter()
                .filter(|item| {
                    item["custom_id"]
                        .as_str()
                        .map(|cid| cid.starts_with("ask_cancel:"))
                        .unwrap_or(false)
                })
                .count()
        });
        assert_eq!(count, 1, "expected exactly 1 ask_cancel: button when no options present");
    }

    // ── cancel button parsing ───────────────────────────────────────────────

    #[test]
    fn parse_question_cancel_button_valid() {
        let rid = parse_question_cancel_button_id("ask_cancel:req-1").unwrap();
        assert_eq!(rid, "req-1");
    }

    #[test]
    fn parse_question_cancel_button_rejects_confirm_prefix() {
        // "ask_cancel_confirm:" must NOT be matched by parse_question_cancel_button_id
        assert!(
            parse_question_cancel_button_id("ask_cancel_confirm:req-1").is_none(),
            "ask_cancel_confirm: should not match ask_cancel: parser"
        );
    }

    #[test]
    fn parse_question_cancel_button_rejects_abort_prefix() {
        // "ask_cancel_abort:" must NOT be matched by parse_question_cancel_button_id
        assert!(
            parse_question_cancel_button_id("ask_cancel_abort:req-1").is_none(),
            "ask_cancel_abort: should not match ask_cancel: parser"
        );
    }

    #[test]
    fn cancel_custom_ids_within_100_chars() {
        // Longest prefix is "ask_cancel_confirm:" (19 chars) + 80-char rid = 99 chars ≤ 100
        let rid = "a".repeat(80);
        assert!(
            format!("ask_cancel:{}", rid).len() <= 100,
            "ask_cancel: + 80-char rid must be within 100 chars"
        );
        assert!(
            format!("ask_cancel_confirm:{}", rid).len() <= 100,
            "ask_cancel_confirm: + 80-char rid must be within 100 chars"
        );
        assert!(
            format!("ask_cancel_abort:{}", rid).len() <= 100,
            "ask_cancel_abort: + 80-char rid must be within 100 chars"
        );
    }
}
