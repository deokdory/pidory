use poise::serenity_prelude::{
    ButtonStyle, ChannelId, Context, CreateActionRow, CreateButton, CreateInputText, CreateMessage,
    CreateModal, CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption, EditMessage,
    InputTextStyle, MessageId, UserId,
};
use serde_json::Value;

use crate::error::PidoryError;
use crate::i18n::Lang;

/// Creates a Discord message displaying a question with interactive components.
/// - 2-5 options → Buttons (+ free text button)
/// - 6-25 options → Select Menu (+ free text button)
/// - No options → Free text button only
pub fn create_question_message(
    input: &Value,
    request_id: &str,
    triggered_by: UserId,
    lang: Lang,
) -> CreateMessage {
    let question_count = input
        .get("questions")
        .and_then(|q| q.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    if question_count > 1 {
        tracing::warn!("AskUserQuestion has {} questions, only first will be shown", question_count);
    }

    let question = extract_first_question(input);
    let q_text = question.get("question").and_then(|v| v.as_str()).unwrap_or("");
    let header = question.get("header").and_then(|v| v.as_str()).unwrap_or("");
    let options: Vec<(String, String)> = extract_options(&question);

    let header_text = if header.is_empty() {
        String::new()
    } else {
        format!(" — **{}**", header)
    };

    let content = format!("<@{}> ❓{}\n{}", triggered_by, header_text, q_text);

    let mut components = Vec::new();

    if options.len() >= 6 {
        let capped_options = if options.len() > 25 {
            tracing::warn!("AskUserQuestion has {} options, capping to 25 for Discord select menu", options.len());
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
            format!("ask_sel:{}", truncate_request_id(request_id)),
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
                CreateButton::new(format!("ask:{}:{}", truncate_request_id(request_id), i))
                    .label(label)
                    .style(ButtonStyle::Primary)
            })
            .collect();
        components.push(CreateActionRow::Buttons(buttons));
    }

    let text_btn = CreateButton::new(format!("ask_text:{}", truncate_request_id(request_id)))
        .label(lang.question_write_answer())
        .style(ButtonStyle::Secondary)
        .emoji('✏');
    components.push(CreateActionRow::Buttons(vec![text_btn]));

    CreateMessage::new().content(content).components(components)
}

fn extract_first_question(input: &Value) -> Value {
    input
        .get("questions")
        .and_then(|q| q.as_array())
        .and_then(|arr| arr.first())
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

/// Truncates request_id to 80 chars, leaving room for prefixes and suffixes within Discord's 100-char custom_id limit.
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

/// Resolves an option label from the original input and option index.
pub fn resolve_option_label(input: &Value, option_index: usize) -> String {
    let question = extract_first_question(input);
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
    let edit = EditMessage::new().content(label).components(vec![]);
    channel_id
        .edit_message(ctx, message_id, edit)
        .await
        .map_err(|e| PidoryError::Discord(Box::new(e)))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_question_button_valid() {
        let (rid, idx) = parse_question_button_id("ask:abc-123:2").unwrap();
        assert_eq!(rid, "abc-123");
        assert_eq!(idx, 2);
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

    #[test]
    fn resolve_option_label_valid() {
        let input = serde_json::json!({
            "questions": [{"question": "pick", "options": [
                {"label": "A", "description": "aa"},
                {"label": "B", "description": "bb"}
            ]}]
        });
        assert_eq!(resolve_option_label(&input, 0), "A");
        assert_eq!(resolve_option_label(&input, 1), "B");
    }

    #[test]
    fn resolve_option_label_out_of_bounds() {
        let input =
            serde_json::json!({"questions": [{"question": "q", "options": [{"label": "X"}]}]});
        assert_eq!(resolve_option_label(&input, 5), "5");
    }

    #[test]
    fn extract_first_question_empty() {
        let input = serde_json::json!({});
        let q = extract_first_question(&input);
        assert!(q.is_object());
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

    #[test]
    fn create_question_message_multi_question_uses_first() {
        let input = serde_json::json!({
            "questions": [
                {"question": "First?", "options": [{"label": "A"}]},
                {"question": "Second?", "options": [{"label": "B"}]}
            ]
        });
        let msg = create_question_message(&input, "req-1", UserId::new(1), Lang::En);
        let json = serde_json::to_value(&msg).unwrap();
        let content = json["content"].as_str().unwrap_or("");
        assert!(content.contains("First?"));
        assert!(!content.contains("Second?"));
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
}
