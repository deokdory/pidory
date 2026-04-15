use std::path::Path;

use poise::serenity_prelude as serenity;

use crate::{Context, Error};
use crate::db::repository;

async fn autocomplete_path(
    ctx: Context<'_>,
    partial: &str,
) -> Vec<poise::serenity_prelude::AutocompleteChoice> {
    let partial = strip_display_prefix(partial);
    let project_roots = ctx.data().config.discord.project_roots.clone();

    if project_roots.is_empty() {
        return Vec::new();
    }

    // Helper: check if a path is under one of the project_roots
    let is_under_roots = |p: &std::path::Path| -> bool {
        project_roots.iter().any(|root| p.starts_with(root))
    };

    // Helper: build a display string for a path
    let make_display = |p: &std::path::Path| -> String {
        let last = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_else(|| p.to_str().unwrap_or(""));
        let full = p.to_str().unwrap_or("");
        let combined = format!("{} \u{2014} {}", last, full);
        if combined.chars().count() > 100 {
            let truncated: String = combined.chars().take(97).collect();
            format!("{}...", truncated)
        } else {
            combined
        }
    };

    if partial.is_empty() {
        // Return project_roots themselves, each with trailing slash
        return project_roots
            .iter()
            .filter_map(|root| {
                let value = if root.ends_with('/') {
                    root.clone()
                } else {
                    format!("{}/", root)
                };
                if value.len() > 100 {
                    return None;
                }
                let display = make_display(std::path::Path::new(root));
                Some(poise::serenity_prelude::AutocompleteChoice::new(display, value))
            })
            .take(25)
            .collect();
    }

    // Determine the directory to list and optional filter prefix
    let (list_dir, filter_prefix): (String, Option<String>) = if partial.ends_with('/') {
        (partial.to_string(), None)
    } else {
        let p = std::path::Path::new(partial);
        let parent = p
            .parent()
            .map(|par| {
                let s = par.to_str().unwrap_or("");
                if s.is_empty() {
                    "/".to_string()
                } else if s.ends_with('/') {
                    s.to_string()
                } else {
                    format!("{}/", s)
                }
            })
            .unwrap_or_else(|| "/".to_string());
        let stem = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        (parent, Some(stem))
    };

    // Verify list_dir is under a project root before reading filesystem
    let list_dir_canonical = tokio::fs::canonicalize(&list_dir).await
        .unwrap_or_else(|_| std::path::PathBuf::from(&list_dir));
    if !is_under_roots(&list_dir_canonical) {
        return Vec::new();
    }

    let mut rd = match tokio::fs::read_dir(&list_dir).await {
        Ok(rd) => rd,
        Err(_) => {
            return Vec::new();
        }
    };

    let mut choices: Vec<poise::serenity_prelude::AutocompleteChoice> = Vec::new();

    while let Ok(Some(entry)) = rd.next_entry().await {
        let meta = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_dir() {
            continue;
        }
        let file_name = entry.file_name();
        let name_str = match file_name.to_str() {
            Some(s) => s,
            None => continue,
        };

        // Apply filter prefix if any
        if let Some(ref prefix) = filter_prefix && !name_str.starts_with(prefix.as_str()) {
            continue;
        }

        // Build full path value with trailing slash
        let full_path_buf = std::path::Path::new(&list_dir).join(name_str);
        let full_path = format!("{}/", full_path_buf.to_str().unwrap_or(""));
        if full_path.len() > 100 {
            continue;
        }

        // Verify it's under a project root after canonicalize (best effort)
        let canonical = tokio::fs::canonicalize(&full_path_buf).await
            .unwrap_or(full_path_buf);
        if !is_under_roots(&canonical) {
            continue;
        }

        let choice_name = make_display(&canonical);
        choices.push(poise::serenity_prelude::AutocompleteChoice::new(choice_name, full_path));
        if choices.len() >= 25 {
            break;
        }
    }

    choices
}

#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    required_permissions = "MANAGE_GUILD"
)]
pub async fn register(
    ctx: Context<'_>,
    #[description = "Project directory path"]
    #[autocomplete = "autocomplete_path"]
    path: String,
    #[description = "Display name (optional)"] name: Option<String>,
) -> Result<(), Error> {
    let channel_id = ctx.channel_id().to_string();
    let lang = ctx.data().config.language;

    if !Path::new(&path).exists() {
        let reply = poise::CreateReply::default()
            .content(format!("❌ {}", lang.path_not_exist(&path)))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    if let Some(existing) = repository::get_project_by_channel(&ctx.data().db, &channel_id).await? {
        let reply = poise::CreateReply::default()
            .content(format!("❌ {}", lang.already_registered(&existing.path)))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    repository::register_project(&ctx.data().db, &channel_id, &path, name.as_deref()).await?;

    let reply = poise::CreateReply::default()
        .content(format!("✅ {}", lang.registered(&path)))
        .ephemeral(true);
    ctx.send(reply).await?;

    // Try to update channel topic; ignore failure
    let _ = ctx
        .channel_id()
        .edit(ctx.http(), serenity::EditChannel::new().topic(format!("pidory: {path}")))
        .await;

    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    required_permissions = "MANAGE_GUILD"
)]
pub async fn unregister(ctx: Context<'_>) -> Result<(), Error> {
    let channel_id = ctx.channel_id().to_string();
    let lang = ctx.data().config.language;

    let project =
        repository::get_project_by_channel(&ctx.data().db, &channel_id).await?;

    if project.is_none() {
        let reply = poise::CreateReply::default()
            .content(format!("❌ {}", lang.not_registered()))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    // Clean up all sessions for this channel before removing the project
    let sessions = repository::list_sessions_by_channel(&ctx.data().db, &channel_id).await?;
    for session in &sessions {
        if let Err(e) = ctx.data().sessions.kill_session(&session.thread_id).await {
            tracing::warn!(thread_id = %session.thread_id, "Failed to kill session during unregister: {}", e);
        }
        ctx.data().session_skills.lock().await.remove(&session.thread_id);
        ctx.data().next_step_buttons.lock().await.remove(&session.thread_id);
        ctx.data().pending_permissions.lock().await.retain(|_, p| p.thread_id != session.thread_id);
        ctx.data().needs_context.lock().await.remove(&session.thread_id);
        ctx.data().turn_initiators.lock().await.remove(&session.thread_id);
        ctx.data().turn_participants.lock().await.remove(&session.thread_id);
    }
    repository::delete_sessions_by_channel(&ctx.data().db, &channel_id).await?;

    repository::unregister_project(&ctx.data().db, &channel_id).await?;

    let reply = poise::CreateReply::default()
        .content(format!("✅ {}", lang.unregistered()))
        .ephemeral(true);
    ctx.send(reply).await?;

    Ok(())
}

#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    required_permissions = "MANAGE_GUILD",
)]
pub async fn new_project(
    ctx: Context<'_>,
    #[description = "Project directory path"]
    #[autocomplete = "autocomplete_path"]
    path: String,
    #[description = "Channel name (default: directory name)"] name: Option<String>,
    #[description = "Category ID (default: config)"] category: Option<String>,
) -> Result<(), Error> {
    let lang = ctx.data().config.language;
    let config = &ctx.data().config;

    // 1. Path must exist
    if !Path::new(&path).exists() {
        let reply = poise::CreateReply::default()
            .content(format!("❌ {}", lang.path_not_exist(&path)))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    // 2. Canonicalize (best effort)
    let canonical_path = tokio::fs::canonicalize(&path).await
        .unwrap_or_else(|_| std::path::PathBuf::from(&path));

    // 3. Verify within project_roots if configured
    let project_roots = &config.discord.project_roots;
    if !project_roots.is_empty() {
        let in_roots = project_roots
            .iter()
            .any(|root| canonical_path.starts_with(root));
        if !in_roots {
            let reply = poise::CreateReply::default()
                .content(format!("❌ {}", lang.path_not_in_roots(&path)))
                .ephemeral(true);
            ctx.send(reply).await?;
            return Ok(());
        }
    }

    // Defer the interaction to avoid Discord's 3-second timeout
    ctx.defer_ephemeral().await?;

    // 4. Determine channel name
    let raw_name = name.as_deref()
        .map(String::from)
        .or_else(|| {
            Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from)
        });
    let channel_name = match raw_name.as_deref().and_then(sanitize_channel_name) {
        Some(n) => n,
        None => {
            let reply = poise::CreateReply::default()
                .content(format!(
                    "❌ {} {}",
                    lang.channel_name_invalid(),
                    lang.channel_name_specify_hint()
                ))
                .ephemeral(true);
            ctx.send(reply).await?;
            return Ok(());
        }
    };

    // 5. Resolve category
    let category_id: Option<serenity::ChannelId> = match category
        .as_deref()
        .or(config.discord.default_category_id.as_deref())
    {
        Some(id_str) => match id_str.parse::<u64>() {
            Ok(id) => Some(serenity::ChannelId::new(id)),
            Err(_) => {
                let reply = poise::CreateReply::default()
                    .content(format!("❌ {}", lang.category_not_found()))
                    .ephemeral(true);
                ctx.send(reply).await?;
                return Ok(());
            }
        },
        None => None,
    };

    // 6. Get guild ID
    let guild_id = match ctx.guild_id() {
        Some(id) => id,
        None => {
            let reply = poise::CreateReply::default()
                .content("❌ Not in a guild")
                .ephemeral(true);
            ctx.send(reply).await?;
            return Ok(());
        }
    };

    // 7. Create the channel
    let mut builder = serenity::CreateChannel::new(&channel_name)
        .kind(serenity::ChannelType::Text)
        .topic(format!("pidory: {path}"));
    if let Some(cat) = category_id {
        builder = builder.category(cat);
    }

    let new_channel = match guild_id.create_channel(ctx.http(), builder).await {
        Ok(ch) => ch,
        Err(e) => {
            tracing::warn!("Failed to create channel: {}", e);
            let reply = poise::CreateReply::default()
                .content(format!("❌ {}", lang.channel_create_failed()))
                .ephemeral(true);
            ctx.send(reply).await?;
            return Ok(());
        }
    };

    let new_channel_id = new_channel.id.to_string();

    // 8. Register in DB
    let canonical_str = canonical_path.to_string_lossy();
    match repository::register_project(
        &ctx.data().db,
        &new_channel_id,
        &canonical_str,
        name.as_deref(),
    )
    .await
    {
        Ok(_) => {
            let reply = poise::CreateReply::default()
                .content(format!(
                    "✅ {}",
                    lang.new_project_created(&new_channel_id, &path)
                ))
                .ephemeral(true);
            ctx.send(reply).await?;
        }
        Err(e) => {
            tracing::error!("Channel created but DB registration failed: {}", e);
            let reply = poise::CreateReply::default()
                .content(format!(
                    "⚠️ {}",
                    lang.channel_created_but_register_failed(&new_channel_id)
                ))
                .ephemeral(true);
            ctx.send(reply).await?;
        }
    }

    Ok(())
}

/// Strip the display prefix added by `make_display` from a partial string.
///
/// When Discord autocomplete inserts a selected choice into the input field it
/// uses the choice *name* (display text), not the value.  `make_display`
/// formats names as `"{last} \u{2014} {full_path}"`.  The next autocomplete
/// invocation therefore receives the display string as `partial`.
///
/// This function detects that pattern and returns the part after the separator,
/// i.e. the raw path / value.  If the pattern is not present the original
/// `partial` is returned unchanged.
pub(crate) fn strip_display_prefix(partial: &str) -> &str {
    // Separator is SPACE + U+2014 (EM DASH, 3 UTF-8 bytes) + SPACE = 5 bytes total.
    const SEP: &str = " \u{2014} ";
    match partial.find(SEP) {
        Some(pos) => &partial[pos + SEP.len()..],
        None => partial,
    }
}

/// Sanitize a string into a valid Discord channel name.
///
/// Rules:
/// 1. Lowercase the input
/// 2. Replace any character not in `[a-z0-9-]` with `-`
/// 3. Collapse consecutive `-` into a single `-`
/// 4. Strip leading and trailing `-`
/// 5. Return `None` if the result is shorter than 2 characters
/// 6. Truncate to 100 characters
pub(crate) fn sanitize_channel_name(name: &str) -> Option<String> {
    let lowered = name.to_lowercase();

    // Replace non-alphanumeric/hyphen with hyphen
    let replaced: String = lowered
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();

    // Collapse consecutive hyphens
    let mut collapsed = String::with_capacity(replaced.len());
    let mut prev_hyphen = false;
    for c in replaced.chars() {
        if c == '-' {
            if !prev_hyphen {
                collapsed.push(c);
            }
            prev_hyphen = true;
        } else {
            collapsed.push(c);
            prev_hyphen = false;
        }
    }

    // Strip leading and trailing hyphens, then truncate to 100 chars
    let trimmed = collapsed.trim_matches('-');
    let truncated: String = trimmed.chars().take(100).collect();

    if truncated.len() < 2 {
        None
    } else {
        Some(truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::{sanitize_channel_name, strip_display_prefix};

    #[test]
    fn basic_sanitize() {
        assert_eq!(sanitize_channel_name("My Project"), Some("my-project".to_string()));
    }

    #[test]
    fn consecutive_hyphens_collapsed() {
        assert_eq!(sanitize_channel_name("foo--bar"), Some("foo-bar".to_string()));
    }

    #[test]
    fn leading_trailing_stripped() {
        assert_eq!(sanitize_channel_name("-hello-"), Some("hello".to_string()));
    }

    #[test]
    fn too_short_returns_none() {
        assert_eq!(sanitize_channel_name("a"), None);
        assert_eq!(sanitize_channel_name(""), None);
        assert_eq!(sanitize_channel_name("---"), None);
    }

    #[test]
    fn exactly_two_chars() {
        assert_eq!(sanitize_channel_name("ab"), Some("ab".to_string()));
    }

    #[test]
    fn truncated_to_100() {
        let long = "a".repeat(200);
        let result = sanitize_channel_name(&long).unwrap();
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn special_chars_replaced() {
        assert_eq!(sanitize_channel_name("hello_world!"), Some("hello-world-".to_string().trim_matches('-').to_string()));
        assert_eq!(sanitize_channel_name("foo/bar/baz"), Some("foo-bar-baz".to_string()));
    }

    // --- strip_display_prefix tests ---

    #[test]
    fn strip_display_format_extracts_path() {
        // make_display produces "{last} \u{2014} {full}" — strip the prefix
        assert_eq!(
            strip_display_prefix("projects \u{2014} /home/user/projects"),
            "/home/user/projects"
        );
    }

    #[test]
    fn strip_plain_path_unchanged() {
        // No separator present — return as-is
        assert_eq!(
            strip_display_prefix("/home/user/projects/"),
            "/home/user/projects/"
        );
    }

    #[test]
    fn strip_empty_string() {
        assert_eq!(strip_display_prefix(""), "");
    }

    #[test]
    fn strip_truncated_display_extracts_path() {
        // Simulates a display string whose name portion was truncated at 100 chars
        // but the separator and path are still present
        assert_eq!(
            strip_display_prefix("longname \u{2014} /some/path"),
            "/some/path"
        );
    }

    #[test]
    fn strip_em_dash_in_path_keeps_remainder() {
        // Only the FIRST separator should be consumed; em dashes inside the path
        // are preserved verbatim
        assert_eq!(
            strip_display_prefix("test \u{2014} /path/with \u{2014} dash"),
            "/path/with \u{2014} dash"
        );
    }
}
