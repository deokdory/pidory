use crate::db::repository;
use crate::{Context, Error};

fn validate_model_name(name: &str) -> Option<String> {
    let normalized = name.to_lowercase();
    match normalized.as_str() {
        "opus" | "sonnet" | "haiku" => Some(normalized),
        s if s.strip_prefix("claude-").is_some_and(|rest| !rest.is_empty()) => Some(normalized),
        _ => None,
    }
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
}

async fn autocomplete_model(
    _ctx: Context<'_>,
    partial: &str,
) -> Vec<poise::serenity_prelude::AutocompleteChoice> {
    let models = ["opus", "sonnet", "haiku"];
    models
        .iter()
        .filter(|m| partial.is_empty() || m.contains(partial))
        .map(|m| poise::serenity_prelude::AutocompleteChoice::new(*m, *m))
        .collect()
}
