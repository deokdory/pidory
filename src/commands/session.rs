use poise::serenity_prelude as serenity;

use crate::{Context, Error};
use crate::db::repository;

/// Parse a naive datetime string "YYYY-MM-DD HH:MM:SS[.f]" into Unix seconds.
/// Returns None if parsing fails.
fn parse_datetime_secs(dt_str: &str) -> Option<i64> {
    // Expected format from SQLite: "2024-01-01 12:00:00" or "2024-01-01 12:00:00.123"
    let s = dt_str.trim();
    let (date_part, time_part) = if let Some(idx) = s.find(' ') {
        (&s[..idx], &s[idx + 1..])
    } else {
        return None;
    };

    let mut date_parts = date_part.splitn(3, '-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;

    let time_no_frac = time_part.split('.').next()?;
    let mut time_parts = time_no_frac.splitn(3, ':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let min: i64 = time_parts.next()?.parse().ok()?;
    let sec: i64 = time_parts.next()?.parse().ok()?;

    // Compute days since Unix epoch using a simple formula (ignores leap seconds).
    // Days from 1970-01-01 to year-month-day using Julian Day Number difference.
    let a = (14 - month) / 12;
    let y = year + 4800 - a;
    let m = month + 12 * a - 3;
    let jdn = day + (153 * m + 2) / 5 + 365 * y + y / 4 - y / 100 + y / 400 - 32045;
    // Julian Day Number of 1970-01-01 is 2440588
    let days_since_epoch = jdn - 2440588;
    Some(days_since_epoch * 86400 + hour * 3600 + min * 60 + sec)
}

/// Format an ISO-8601 datetime string as "Xm ago" relative to now.
/// Falls back to the raw string if parsing fails.
fn format_relative(dt_str: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let then_secs = match parse_datetime_secs(dt_str) {
        Some(s) => s,
        None => return dt_str.to_string(),
    };

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let diff = (now_secs - then_secs).max(0) as u64;
    if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

#[poise::command(slash_command, guild_only, owners_only)]
pub async fn list(
    ctx: Context<'_>,
    #[description = "Channel (defaults to current channel)"] channel: Option<serenity::ChannelId>,
) -> Result<(), Error> {
    let channel_id = channel
        .unwrap_or_else(|| ctx.channel_id())
        .to_string();

    let sessions =
        repository::list_sessions_by_channel(&ctx.data().db, &channel_id).await?;

    let content = if sessions.is_empty() {
        "No active sessions".to_string()
    } else {
        let mut lines = vec!["📋 Active Sessions:".to_string()];
        for s in &sessions {
            let thread_mention = format!("<#{}>", s.thread_id);
            let since = s
                .last_active_at
                .as_deref()
                .map(|t| format!(" — since: {}", format_relative(t)))
                .unwrap_or_default();
            let session_short = s
                .session_id
                .as_deref()
                .map(|id| format!(" — session: {}…", &id[..id.len().min(8)]))
                .unwrap_or_default();
            lines.push(format!(
                "• {} — status: {}{}{}",
                thread_mention, s.status, session_short, since
            ));
        }
        lines.join("\n")
    };

    let reply = poise::CreateReply::default()
        .content(content)
        .ephemeral(true);
    ctx.send(reply).await?;

    Ok(())
}

#[poise::command(slash_command, guild_only, owners_only)]
pub async fn del(
    ctx: Context<'_>,
    #[description = "Thread ID (defaults to current thread)"] thread_id: Option<String>,
) -> Result<(), Error> {
    let tid = match thread_id {
        Some(id) => id,
        None => {
            // Must be inside a thread
            match ctx.channel_id().to_channel(ctx.http()).await? {
                serenity::Channel::Guild(ch) if ch.thread_metadata.is_some() => {
                    ctx.channel_id().to_string()
                }
                _ => {
                    let reply = poise::CreateReply::default()
                        .content("❌ Not in a thread. Provide a thread ID explicitly.")
                        .ephemeral(true);
                    ctx.send(reply).await?;
                    return Ok(());
                }
            }
        }
    };

    let session = repository::get_session_by_thread(&ctx.data().db, &tid).await?;
    if session.is_none() {
        let reply = poise::CreateReply::default()
            .content(format!("❌ No session found for thread `{tid}`"))
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    // Kill session if running (ignore failure — process may have already exited)
    let _ = ctx.data().sessions.kill_session(&tid).await;

    repository::delete_session(&ctx.data().db, &tid).await?;

    let reply = poise::CreateReply::default()
        .content("✅ Session deleted")
        .ephemeral(true);
    ctx.send(reply).await?;

    Ok(())
}

/// 진행 중인 Claude Code 작업 중단
#[poise::command(slash_command, guild_only, owners_only)]
pub async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    let channel_id = ctx.channel_id();
    let thread_id = channel_id.to_string();
    let data = ctx.data();

    if !data.sessions.session_exists(&thread_id).await {
        ctx.say("❌ 이 스레드에 활성 세션이 없습니다").await?;
        return Ok(());
    }

    match data.sessions.interrupt_session(&thread_id).await {
        Ok(()) => {
            ctx.say("-# ⛔ Interrupted").await?;
        }
        Err(e) => {
            ctx.say(format!("❌ 중단 실패: {}", e)).await?;
        }
    }

    Ok(())
}

#[poise::command(slash_command, guild_only, owners_only)]
pub async fn status(
    ctx: Context<'_>,
    #[description = "Thread ID (defaults to current thread)"] thread_id: Option<String>,
) -> Result<(), Error> {
    let tid = match thread_id {
        Some(id) => id,
        None => ctx.channel_id().to_string(),
    };

    let session = repository::get_session_by_thread(&ctx.data().db, &tid).await?;

    let content = match session {
        None => format!("❌ No session found for thread `{tid}`"),
        Some(s) => {
            let session_id_line = s
                .session_id
                .as_deref()
                .unwrap_or("(none)");
            let last_active_line = s
                .last_active_at
                .as_deref()
                .map(format_relative)
                .unwrap_or_else(|| "(never)".to_string());
            format!(
                "📊 Session Status\nThread: <#{}>\nStatus: {}\nSession ID: {}\nLast Active: {}",
                s.thread_id, s.status, session_id_line, last_active_line
            )
        }
    };

    let reply = poise::CreateReply::default()
        .content(content)
        .ephemeral(true);
    ctx.send(reply).await?;

    Ok(())
}
