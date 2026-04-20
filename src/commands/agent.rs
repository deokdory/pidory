use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::db::repository;
use crate::handler::message;
use crate::{Context, Error};

/// `~/.claude/agents/*.md` 파일에서 (name, description) 맵 로드
pub fn load_global_agent_descriptions() -> HashMap<String, String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let agents_path = Path::new(&home).join(".claude/agents");
    load_agent_descriptions_from_dir(&agents_path)
}

/// `<project_path>/.claude/agents/*.md` 파일에서 (name, description) 맵 로드.
/// 경로가 없거나 에러가 나면 빈 맵 반환 (panic 없음).
pub fn load_project_agent_descriptions(project_path: &str) -> HashMap<String, String> {
    let agents_path = Path::new(project_path).join(".claude/agents");
    load_agent_descriptions_from_dir(&agents_path)
}

fn load_agent_descriptions_from_dir(agents_path: &Path) -> HashMap<String, String> {
    let mut descriptions = HashMap::new();

    let entries = match fs::read_dir(agents_path) {
        Ok(e) => e,
        Err(_) => return descriptions,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            if let Some((name, desc)) = parse_frontmatter_fields(&content) {
                descriptions.insert(name, desc);
            }
        }
    }

    descriptions
}

/// 프로젝트 로컬이 글로벌을 덮어쓰는 머지. 순수 함수.
pub fn merge_agent_descriptions(
    global: HashMap<String, String>,
    local: HashMap<String, String>,
) -> HashMap<String, String> {
    let mut merged = global;
    for (k, v) in local {
        merged.insert(k, v);
    }
    merged
}

/// frontmatter에서 `name`과 `description` 필드를 추출.
/// 둘 중 하나라도 없으면 `None`. 순수 함수.
pub fn parse_frontmatter_fields(content: &str) -> Option<(String, String)> {
    let mut lines = content.lines();

    if lines.next()?.trim() != "---" {
        return None;
    }

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("name:") {
            let val = rest.trim().trim_matches('"').trim_matches('\'');
            name = Some(val.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("description:") {
            let val = rest.trim().trim_matches('"').trim_matches('\'');
            description = Some(val.to_string());
        }
    }

    match (name, description) {
        (Some(n), Some(d)) => Some((n, d)),
        _ => None,
    }
}

/// Claude Code agent 실행
#[poise::command(slash_command, guild_only)]
pub async fn agent(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_agent"]
    #[description = "실행할 agent"]
    name: String,
    #[description = "agent에게 맡길 작업"]
    #[rest]
    task: String,
) -> Result<(), Error> {
    let content = format!("Use the {} subagent proactively to: {}", name, task);

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

    // 초기 응답 전송
    let reply = ctx.say(format!("-# /agent {}", name)).await?;
    let msg = reply.into_message().await?;
    let msg_id = msg.id;

    message::execute_in_session(
        serenity_ctx,
        data,
        &thread_id,
        channel_id,
        msg_id,
        &content,
        ctx.author().id,
    )
    .await?;

    Ok(())
}

async fn autocomplete_agent<'a>(
    ctx: Context<'_>,
    partial: &'a str,
) -> Vec<poise::serenity_prelude::AutocompleteChoice> {
    let data = ctx.data();
    let thread_id = ctx.channel_id().to_string();

    // 글로벌 descriptions 기준으로 시작
    let global = data.agent_descriptions.clone();

    // 프로젝트 로컬 로드 시도 (에러 시 글로벌만 사용)
    let descriptions = 'lookup: {
        let session = match repository::get_session_by_thread(&data.db, &thread_id).await {
            Ok(Some(s)) => s,
            _ => break 'lookup global,
        };
        let project = match repository::get_project_by_channel(&data.db, &session.channel_id).await {
            Ok(Some(p)) => p,
            _ => break 'lookup global,
        };
        let local = load_project_agent_descriptions(&project.path);
        merge_agent_descriptions(global, local)
    };

    descriptions
        .into_iter()
        .filter(|(name, _): &(String, String)| partial.is_empty() || name.contains(partial))
        .map(|(name, desc)| {
            let combined = format!("{} \u{2014} {}", name, desc);
            let display = if combined.chars().count() > 100 {
                let truncated: String = combined.chars().take(97).collect();
                format!("{}...", truncated)
            } else {
                combined
            };
            poise::serenity_prelude::AutocompleteChoice::new(display, name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_fields_both_present() {
        let content = "---\nname: implementer\ndescription: 구현 엔지니어\nmodel: sonnet\n---\n\nbody";
        let result = parse_frontmatter_fields(content);
        assert_eq!(
            result,
            Some(("implementer".to_string(), "구현 엔지니어".to_string()))
        );
    }

    #[test]
    fn parse_frontmatter_fields_no_frontmatter() {
        let content = "Just plain text without frontmatter";
        let result = parse_frontmatter_fields(content);
        assert!(result.is_none());
    }

    #[test]
    fn parse_frontmatter_fields_missing_name() {
        let content = "---\ndescription: some description\nmodel: sonnet\n---\n\nbody";
        let result = parse_frontmatter_fields(content);
        assert!(result.is_none());
    }

    #[test]
    fn parse_frontmatter_fields_missing_description() {
        let content = "---\nname: researcher\nmodel: sonnet\n---\n\nbody";
        let result = parse_frontmatter_fields(content);
        assert!(result.is_none());
    }

    #[test]
    fn merge_agent_descriptions_no_conflict() {
        let mut global = HashMap::new();
        global.insert("architect".to_string(), "아키텍트".to_string());
        global.insert("implementer".to_string(), "구현".to_string());

        let mut local = HashMap::new();
        local.insert("custom-agent".to_string(), "커스텀".to_string());

        let merged = merge_agent_descriptions(global, local);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged.get("architect"), Some(&"아키텍트".to_string()));
        assert_eq!(merged.get("implementer"), Some(&"구현".to_string()));
        assert_eq!(merged.get("custom-agent"), Some(&"커스텀".to_string()));
    }

    #[test]
    fn merge_agent_descriptions_local_overrides_global() {
        let mut global = HashMap::new();
        global.insert("implementer".to_string(), "글로벌 구현".to_string());

        let mut local = HashMap::new();
        local.insert("implementer".to_string(), "프로젝트 커스텀 구현".to_string());

        let merged = merge_agent_descriptions(global, local);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged.get("implementer"),
            Some(&"프로젝트 커스텀 구현".to_string())
        );
    }

    #[test]
    fn parse_frontmatter_fields_quoted_values() {
        let content = "---\nname: \"my-agent\"\ndescription: 'quoted desc'\n---\n";
        let result = parse_frontmatter_fields(content);
        assert_eq!(
            result,
            Some(("my-agent".to_string(), "quoted desc".to_string()))
        );
    }
}
