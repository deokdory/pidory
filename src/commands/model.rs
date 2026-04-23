use crate::db::repository;
use crate::{Context, Error};

/// (autocomplete label, CLI model ID). label은 `shorten_model_name()` 출력과 일치.
const VERSIONED_MODELS: &[(&str, &str)] = &[
    ("opus 4.7", "claude-opus-4-7"),
    ("opus 4.6", "claude-opus-4-6"),
    ("sonnet 4.6", "claude-sonnet-4-6"),
    ("haiku 4.5", "claude-haiku-4-5"),
];

const SHORT_ALIASES: &[&str] = &["opus", "sonnet", "haiku"];

fn validate_model_name(name: &str) -> Option<String> {
    let normalized = name.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    // 1) short alias
    if SHORT_ALIASES.iter().any(|s| *s == normalized) {
        return Some(normalized);
    }
    // 2) versioned label (opus 4.7 등) → CLI ID
    for (label, cli_id) in VERSIONED_MODELS {
        if *label == normalized {
            return Some((*cli_id).to_string());
        }
    }
    // 3) claude-* full ID (기존 동작)
    if normalized
        .strip_prefix("claude-")
        .is_some_and(|rest| !rest.is_empty())
    {
        return Some(normalized);
    }
    None
}

/// 현재 세션의 모델을 변경합니다
#[poise::command(slash_command, guild_only)]
pub async fn model(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_model"]
    #[description = "모델 이름 (opus, sonnet, haiku)"]
    name: Option<String>,
) -> Result<(), Error> {
    let data = ctx.data();
    let lang = data.config.language;
    let thread_id = ctx.channel_id().to_string();

    match name {
        None => {
            // Getter 모드
            let session = repository::get_session_by_thread(&data.db, &thread_id).await?;
            match session {
                None => {
                    ctx.send(
                        poise::CreateReply::default()
                            .content(format!("❌ {}", lang.no_session_in_thread()))
                            .ephemeral(true),
                    )
                    .await?;
                }
                Some(s) => {
                    let current_model = s.model.as_deref().unwrap_or("default");
                    ctx.send(
                        poise::CreateReply::default()
                            .content(lang.model_current(current_model))
                            .ephemeral(true),
                    )
                    .await?;
                }
            }
        }
        Some(name) => {
            // Setter 모드
            let validated = match validate_model_name(&name) {
                Some(v) => v,
                None => {
                    ctx.send(
                        poise::CreateReply::default()
                            .content(format!("❌ {}", lang.model_invalid(&name)))
                            .ephemeral(true),
                    )
                    .await?;
                    return Ok(());
                }
            };

            let session = repository::get_session_by_thread(&data.db, &thread_id).await?;
            let session = match session {
                None => {
                    ctx.send(
                        poise::CreateReply::default()
                            .content(format!("❌ {}", lang.no_session_in_thread()))
                            .ephemeral(true),
                    )
                    .await?;
                    return Ok(());
                }
                Some(s) => s,
            };

            // 활성 턴 체크
            let is_active = data
                .sessions
                .get_session_info()
                .await
                .into_iter()
                .find(|info| info.thread_id == thread_id)
                .map(|info| info.is_turn_active)
                .unwrap_or(false);

            if is_active {
                ctx.send(
                    poise::CreateReply::default()
                        .content(format!("❌ {}", lang.model_turn_active()))
                        .ephemeral(true),
                )
                .await?;
                return Ok(());
            }

            let old_model = session.model.as_deref().unwrap_or("default").to_string();

            // kill은 best-effort — 이미 없는 세션이면 무시
            if let Err(e) = data.sessions.kill_session(&thread_id).await {
                tracing::debug!(thread_id = %thread_id, error = %e, "kill_session failed (best-effort)");
            }

            repository::update_session_model(&data.db, &thread_id, &validated).await?;

            ctx.send(
                poise::CreateReply::default()
                    .content(format!("✅ {}", lang.model_changed(&old_model, &validated)))
                    .ephemeral(true),
            )
            .await?;
        }
    }

    Ok(())
}

async fn autocomplete_model(
    _ctx: Context<'_>,
    partial: &str,
) -> Vec<poise::serenity_prelude::AutocompleteChoice> {
    let needle = partial.to_ascii_lowercase();
    let candidates: Vec<&str> = SHORT_ALIASES
        .iter()
        .copied()
        .chain(VERSIONED_MODELS.iter().map(|(label, _)| *label))
        .collect();
    candidates
        .into_iter()
        .filter(|label| needle.is_empty() || label.to_ascii_lowercase().contains(&needle))
        .take(25) // Discord slash-command autocomplete 최대 25개 제한
        .map(|label| poise::serenity_prelude::AutocompleteChoice::new(label, label))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Scenario 3: validate_model_name

    #[test]
    fn validate_known_short_names() {
        assert_eq!(validate_model_name("opus"), Some("opus".to_string()));
        assert_eq!(validate_model_name("sonnet"), Some("sonnet".to_string()));
        assert_eq!(validate_model_name("haiku"), Some("haiku".to_string()));
    }

    #[test]
    fn validate_uppercase_normalizes_to_lowercase() {
        assert_eq!(validate_model_name("OPUS"), Some("opus".to_string()));
        assert_eq!(validate_model_name("Sonnet"), Some("sonnet".to_string()));
        assert_eq!(validate_model_name("HAIKU"), Some("haiku".to_string()));
    }

    #[test]
    fn validate_claude_prefix_accepted() {
        assert_eq!(
            validate_model_name("claude-sonnet-4-6"),
            Some("claude-sonnet-4-6".to_string())
        );
        assert_eq!(
            validate_model_name("claude-opus-4-5"),
            Some("claude-opus-4-5".to_string())
        );
        assert_eq!(
            validate_model_name("claude-haiku-3-5"),
            Some("claude-haiku-3-5".to_string())
        );
    }

    #[test]
    fn validate_claude_prefix_uppercase_normalizes() {
        assert_eq!(
            validate_model_name("Claude-Sonnet-4-6"),
            Some("claude-sonnet-4-6".to_string())
        );
    }

    #[test]
    fn validate_unknown_model_returns_none() {
        assert_eq!(validate_model_name("badmodel"), None);
        assert_eq!(validate_model_name("gpt-4"), None);
        assert_eq!(validate_model_name("gemini"), None);
        assert_eq!(validate_model_name(""), None);
    }

    #[test]
    fn validate_partial_claude_prefix_rejected() {
        assert_eq!(validate_model_name("claude"), None);
    }

    #[test]
    fn validate_bare_claude_dash_rejected() {
        assert_eq!(validate_model_name("claude-"), None);
        assert_eq!(validate_model_name("CLAUDE-"), None);
    }

    #[test]
    fn validate_versioned_opus_4_7() {
        assert_eq!(
            validate_model_name("opus 4.7"),
            Some("claude-opus-4-7".to_string())
        );
    }

    #[test]
    fn validate_versioned_case_insensitive() {
        assert_eq!(
            validate_model_name("OPUS 4.7"),
            Some("claude-opus-4-7".to_string())
        );
        assert_eq!(
            validate_model_name("Opus 4.7"),
            Some("claude-opus-4-7".to_string())
        );
    }

    #[test]
    fn validate_versioned_opus_4_6() {
        assert_eq!(
            validate_model_name("opus 4.6"),
            Some("claude-opus-4-6".to_string())
        );
    }

    #[test]
    fn validate_versioned_sonnet_4_6() {
        assert_eq!(
            validate_model_name("sonnet 4.6"),
            Some("claude-sonnet-4-6".to_string())
        );
    }

    #[test]
    fn validate_versioned_haiku_4_5() {
        assert_eq!(
            validate_model_name("haiku 4.5"),
            Some("claude-haiku-4-5".to_string())
        );
    }

    #[test]
    fn validate_versioned_unknown_version_rejected() {
        assert_eq!(validate_model_name("opus 9.9"), None);
    }

    #[test]
    fn validate_versioned_invalid_format_rejected() {
        assert_eq!(validate_model_name("opus 4"), None);
        assert_eq!(validate_model_name("opus 4.x"), None);
    }

    #[test]
    fn round_trip_versioned_to_cli_and_back() {
        use crate::handler::message::shorten_model_name;
        let cli_id = validate_model_name("opus 4.7").expect("should validate");
        assert_eq!(shorten_model_name(&cli_id), "opus 4.7");
    }

    #[test]
    fn autocomplete_candidates_within_discord_limit() {
        // Discord slash-command autocomplete는 최대 25개까지만 받음.
        // 테이블 확장 시 컴파일 타임에 한계 초과를 감지하도록 보장.
        assert!(SHORT_ALIASES.len() + VERSIONED_MODELS.len() <= 25);
    }
}
