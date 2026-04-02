use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::handler::message;
use crate::{Context, Error};

pub fn load_skill_descriptions() -> HashMap<String, String> {
    let mut descriptions = HashMap::new();

    let home = std::env::var("HOME").unwrap_or_default();
    let skills_path = Path::new(&home).join(".claude/skills");

    let entries = match fs::read_dir(&skills_path) {
        Ok(e) => e,
        Err(_) => return descriptions,
    };

    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let skill_name = entry.file_name().to_string_lossy().to_string();

        if let Ok(files) = fs::read_dir(entry.path()) {
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if let Some(desc) = parse_frontmatter_description(&content) {
                            descriptions.insert(skill_name.clone(), desc);
                        }
                    }
                    break;
                }
            }
        }
    }

    descriptions
}

fn parse_frontmatter_description(content: &str) -> Option<String> {
    let mut lines = content.lines();

    if lines.next()?.trim() != "---" {
        return None;
    }

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("description:") {
            let desc = rest.trim().trim_matches('"').trim_matches('\'');
            return Some(desc.to_string());
        }
    }

    None
}

/// Claude Code skill 실행
#[poise::command(slash_command, guild_only, owners_only)]
pub async fn skill(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_skill"]
    #[description = "실행할 skill"]
    name: String,
    #[description = "추가 인자"]
    #[rest]
    args: Option<String>,
) -> Result<(), Error> {
    let content = match args {
        Some(a) if !a.is_empty() => format!("/{} {}", name, a),
        _ => format!("/{}", name),
    };

    let channel_id = ctx.channel_id();
    let thread_id = channel_id.to_string();
    let data = ctx.data();
    let lang = data.config.language;
    let serenity_ctx = ctx.serenity_context();

    // 세션 존재 확인
    if !data.sessions.session_exists(&thread_id).await {
        ctx.say(format!("❌ {}", lang.no_session_in_thread())).await?;
        return Ok(());
    }

    // 초기 응답 전송 — msg_id를 emoji reaction에 사용
    let reply = ctx.say(format!("-# /{}", name)).await?;
    let msg = reply.into_message().await?;
    let msg_id = msg.id;

    message::execute_in_session(
        serenity_ctx,
        data,
        &thread_id,
        channel_id,
        msg_id,
        &content,
    )
    .await?;

    Ok(())
}

async fn autocomplete_skill<'a>(
    ctx: Context<'_>,
    partial: &'a str,
) -> Vec<poise::serenity_prelude::AutocompleteChoice> {
    let thread_id = ctx.channel_id().to_string();
    let skills = ctx
        .data()
        .session_skills
        .lock()
        .await
        .get(&thread_id)
        .cloned()
        .unwrap_or_default();

    let descriptions = &ctx.data().skill_descriptions;

    skills
        .into_iter()
        .filter(move |s| partial.is_empty() || s.contains(partial))
        .map(|s| {
            let display = match descriptions.get(&s) {
                Some(desc) => {
                    let combined = format!("{} \u{2014} {}", s, desc);
                    if combined.chars().count() > 100 {
                        let truncated: String = combined.chars().take(97).collect();
                        format!("{}...", truncated)
                    } else {
                        combined
                    }
                }
                None => s.clone(),
            };
            poise::serenity_prelude::AutocompleteChoice::new(display, s)
        })
        .collect()
}
