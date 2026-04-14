use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use poise::serenity_prelude::{self as serenity, ChannelType, CreateThread, EditThread};
use tokio::sync::mpsc;

use crate::{Context, Data, Error};
use crate::db::repository;
use crate::error::PidoryError;
use crate::handler::formatter;
use crate::subprocess::parser::{ContentBlock, StreamEvent};
use crate::subprocess::session_manager::QueuedMessage;

/// 현재 세션의 컨텍스트를 요약하여 새 Discord 스레드 + Claude Code 세션을 생성
#[poise::command(slash_command, guild_only)]
pub async fn branch(
    ctx: Context<'_>,
    #[description = "분기할 작업의 추가 컨텍스트"]
    #[rest]
    context: Option<String>,
) -> Result<(), Error> {
    let data = ctx.data();
    let lang = data.config.language;
    let serenity_ctx = ctx.serenity_context();
    let channel_id = ctx.channel_id();
    let thread_id = channel_id.to_string();
    let db = &data.db;

    // Discord 3초 interaction deadline 준수: validation 전에 즉시 defer
    ctx.defer_ephemeral().await?;

    // ── Validation ──

    // 1. 스레드인지 확인
    let channel = channel_id
        .to_channel(serenity_ctx)
        .await
        .map_err(|e| PidoryError::Discord(Box::new(e)))?;

    let guild_channel = match channel.guild() {
        Some(gc) if gc.thread_metadata.is_some() => gc,
        _ => {
            ctx.send(
                poise::CreateReply::default()
                    .content(format!("❌ {}", lang.branch_not_in_thread())),
            )
            .await?;
            return Ok(());
        }
    };

    let source_thread_name = guild_channel.name.clone();

    // 2. parent_channel_id 추출
    let parent_channel_id = match guild_channel.parent_id {
        Some(pid) => pid,
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .content(format!("❌ {}", lang.branch_not_in_thread())),
            )
            .await?;
            return Ok(());
        }
    };

    // 3. 프로젝트 등록 확인
    let parent_channel_str = parent_channel_id.to_string();
    let project = match repository::get_project_by_channel(db, &parent_channel_str).await? {
        Some(p) => p,
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .content(format!("❌ {}", lang.branch_no_project())),
            )
            .await?;
            return Ok(());
        }
    };

    // 4. 세션 존재 확인 (DB)
    let session = match repository::get_session_by_thread(db, &thread_id).await? {
        Some(s) => s,
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .content(format!("❌ {}", lang.branch_no_session())),
            )
            .await?;
            return Ok(());
        }
    };

    // 5. 새 세션 슬롯 확인 (요약 전에 — 토큰 낭비 방지)
    if !data.sessions.has_available_slot().await {
        ctx.send(
            poise::CreateReply::default()
                .content(format!(
                    "❌ {}",
                    lang.branch_no_slot(&format!(
                        "{}/{} sessions active",
                        data.sessions.session_count().await,
                        data.config.claude.max_sessions
                    ))
                )),
        )
        .await?;
        return Ok(());
    }

    // 6. 세션 acquire (running이면 거절)
    let acquired = repository::try_acquire_session(db, &thread_id).await?;
    if !acquired {
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.branch_session_busy())),
        )
        .await?;
        return Ok(());
    }

    // ── Phase A: 요약 수집 ──

    let disallowed_tools: Vec<String> = match &project.disallowed_tools {
        Some(json_str) => serde_json::from_str(json_str).unwrap_or_else(|_| {
            data.config.claude.default_disallowed_tools.clone()
        }),
        None => data.config.claude.default_disallowed_tools.clone(),
    };

    // 현재 세션이 SessionManager에 존재하는지 확인 + 재생성
    if let Err(e) = data
        .sessions
        .get_or_create(
            &thread_id,
            &project.path,
            session.session_id.as_deref(),
            &disallowed_tools,
            serenity_ctx.clone(),
            channel_id,
            data.db.clone(),
            lang,
            data.pending_permissions.clone(),
            data.pending_question_groups.clone(),
            data.config.discord.owner_id,
            data.todo_trackers.clone(),
        )
        .await
    {
        let _ = repository::update_session_status(db, &thread_id, "idle").await;
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.branch_summary_failed())),
        )
        .await?;
        return Err(e);
    }

    // 요약 프롬프트 구성
    let extra_context = context.as_deref().unwrap_or("");
    let summary_prompt = lang.branch_summary_prompt(extra_context);

    // 요약 요청 전송 — source thread의 channel_id를 synthetic MessageId로 사용
    let (event_tx, event_rx) = mpsc::channel::<StreamEvent>(64);
    let summary_msg = QueuedMessage {
        content: summary_prompt,
        channel_id,
        message_id: serenity::MessageId::new(channel_id.get()),
        event_tx: Some(event_tx),
        triggered_by: ctx.author().id,
        cancelled: Arc::new(AtomicBool::new(false)),
        downloaded_files: Vec::new(),
        reply_context: None,
    };

    if let Err(e) = data.sessions.send_message(&thread_id, summary_msg).await {
        let _ = repository::update_session_status(db, &thread_id, "idle").await;
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.branch_summary_failed())),
        )
        .await?;
        return Err(e);
    }

    // 응답 수집 (Discord에 출력하지 않음)
    let timeout = data.config.claude.subprocess_timeout_secs;
    let summary_text = match collect_summary_response(event_rx, timeout).await {
        Ok(text) => {
            // Summary turn 완료 — source session 해제
            let _ = repository::update_session_status(db, &thread_id, "idle").await;
            text
        }
        Err(e) => {
            // Timeout → worker가 아직 활성일 수 있으므로 interrupt 먼저
            if e.to_string().contains("timeout") {
                let _ = data.sessions.interrupt_session(&thread_id).await;
            }
            let _ = repository::update_session_status(db, &thread_id, "idle").await;
            ctx.send(
                poise::CreateReply::default()
                    .content(format!("❌ {}", lang.branch_summary_failed())),
            )
            .await?;
            return Ok(());
        }
    };

    // JSON 파싱
    let summary = match parse_summary_response(&summary_text) {
        Some(s) => s,
        None => {
            ctx.send(
                poise::CreateReply::default()
                    .content(format!("❌ {}", lang.branch_summary_failed())),
            )
            .await?;
            return Ok(());
        }
    };

    // ── Phase B: 스레드 생성 + 세션 부트스트랩 ──

    let title = sanitize_thread_title(&summary.title);

    // 부모 채널에 새 스레드 생성
    let new_thread = match parent_channel_id
        .create_thread(
            serenity_ctx,
            CreateThread::new(&title).kind(ChannelType::PublicThread),
        )
        .await
    {
        Ok(thread) => thread,
        Err(_) => {
            ctx.send(
                poise::CreateReply::default()
                    .content(format!("❌ {}", lang.branch_thread_create_failed())),
            )
            .await?;
            return Ok(());
        }
    };

    let new_thread_id = new_thread.id.to_string();
    let new_channel_id = new_thread.id;

    // 새 스레드에 초기 메시지 전송 (bot 메시지 — 사용자가 컨텍스트 확인용)
    let context_header = lang.branch_context_header(&source_thread_name);
    let extra_display = if extra_context.is_empty() {
        String::new()
    } else {
        format!("\n\n**Context:** {}", extra_context)
    };
    let initial_msg_content =
        format!("{}\n\n{}{}", context_header, summary.summary, extra_display);

    // Discord 2000자 제한 대응: split_message로 분할 전송
    let chunks = formatter::split_message(&initial_msg_content, 2000);
    let bot_msg = match new_channel_id
        .say(serenity_ctx, &chunks[0])
        .await
    {
        Ok(msg) => msg,
        Err(e) => {
            tracing::error!("Failed to send initial message to new thread: {}", e);
            cleanup_orphaned_thread(serenity_ctx, data, new_channel_id, &new_thread_id).await;
            ctx.send(
                poise::CreateReply::default()
                    .content(format!("❌ {}", lang.branch_thread_create_failed())),
            )
            .await?;
            return Ok(());
        }
    };
    // 나머지 chunk 전송 (2000자 초과 시)
    for chunk in &chunks[1..] {
        let _ = new_channel_id.say(serenity_ctx, chunk).await;
    }

    // DB 세션 생성
    if let Err(e) = repository::create_session(db, &new_thread_id, &parent_channel_str).await {
        tracing::error!("Failed to create DB session for new thread: {}", e);
        cleanup_orphaned_thread(serenity_ctx, data, new_channel_id, &new_thread_id).await;
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.branch_thread_create_failed())),
        )
        .await?;
        return Ok(());
    }

    // 새 세션 부트스트랩 (Claude CLI 프로세스 생성)
    if let Err(e) = data
        .sessions
        .get_or_create(
            &new_thread_id,
            &project.path,
            None, // 새 세션 — session_id 없음
            &disallowed_tools,
            serenity_ctx.clone(),
            new_channel_id,
            data.db.clone(),
            lang,
            data.pending_permissions.clone(),
            data.pending_question_groups.clone(),
            data.config.discord.owner_id,
            data.todo_trackers.clone(),
        )
        .await
    {
        tracing::error!("Failed to bootstrap new session: {}", e);
        cleanup_orphaned_thread(serenity_ctx, data, new_channel_id, &new_thread_id).await;
        ctx.send(
            poise::CreateReply::default()
                .content(format!(
                    "❌ {}",
                    lang.branch_no_slot(&e.to_string())
                )),
        )
        .await?;
        return Ok(());
    }

    // 새 세션에 컨텍스트 메시지 전송
    let initial_prompt = if extra_context.is_empty() {
        format!(
            "<system-reminder>이 세션은 \"{}\" 스레드에서 분기되었습니다. 아래는 이전 작업의 요약입니다.</system-reminder>\n\n{}\n\nRespond with a single short confirmation that you understood the context. Do NOT use any tools. Do NOT start any work.",
            source_thread_name, summary.summary
        )
    } else {
        format!(
            "<system-reminder>이 세션은 \"{}\" 스레드에서 분기되었습니다. 아래는 이전 작업의 요약입니다.</system-reminder>\n\n{}\n\n{}\n\nRespond with a single short confirmation that you understood the context. Do NOT use any tools. Do NOT start any work.",
            source_thread_name, summary.summary, extra_context
        )
    };

    // 새 세션 acquire — 실패 시 invariant violation, cleanup 후 abort
    let new_acquired = repository::try_acquire_session(db, &new_thread_id).await?;
    if !new_acquired {
        tracing::error!("Failed to acquire newly created session {}", new_thread_id);
        cleanup_orphaned_thread(serenity_ctx, data, new_channel_id, &new_thread_id).await;
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.branch_session_busy())),
        )
        .await?;
        return Ok(());
    }

    let (new_event_tx, new_event_rx) = mpsc::channel::<StreamEvent>(64);
    let new_msg = QueuedMessage {
        content: initial_prompt,
        channel_id: new_channel_id,
        message_id: bot_msg.id,
        event_tx: Some(new_event_tx),
        triggered_by: ctx.author().id,
        cancelled: Arc::new(AtomicBool::new(false)),
        downloaded_files: Vec::new(),
        reply_context: None,
    };

    if let Err(e) = data.sessions.send_message(&new_thread_id, new_msg).await {
        tracing::error!("Failed to send initial message to new session: {}", e);
        cleanup_orphaned_thread(serenity_ctx, data, new_channel_id, &new_thread_id).await;
        ctx.send(
            poise::CreateReply::default()
                .content(format!("❌ {}", lang.branch_summary_failed())),
        )
        .await?;
        return Ok(());
    }

    // ── Phase C: 확인 + 새 세션 응답 스트리밍 ──

    // 원본 스레드에 확인 메시지
    ctx.send(
        poise::CreateReply::default()
            .content(format!(
                "✅ {}",
                lang.branch_thread_created(&format!("<#{}>", new_channel_id))
            )),
    )
    .await?;

    // 새 세션의 초기 turn을 조용히 소비 (Discord에 출력하지 않음)
    let drain_timeout = data.config.claude.subprocess_timeout_secs;
    match drain_initial_turn(
        new_event_rx,
        &new_thread_id,
        db,
        data.session_skills.clone(),
        drain_timeout,
    )
    .await
    {
        Ok(()) => {
            // 준비 완료 알림 (요청자 멘션 포함)
            let mention = format!("<@{}>", ctx.author().id);
            let _ = new_channel_id
                .say(serenity_ctx, &lang.branch_ready(&mention))
                .await;
        }
        Err(e) => {
            tracing::error!("Failed to drain initial turn for {}: {}", new_thread_id, e);
            cleanup_orphaned_thread(serenity_ctx, data, new_channel_id, &new_thread_id).await;
        }
    }

    Ok(())
}

// ── cleanup ──

/// Phase B 실패 시 orphaned thread의 전체 리소스 정리:
/// 1. SessionManager kill (subprocess + worker + pending_recalls)
/// 2. 인메모리 tracking 맵 정리
/// 3. DB 세션 삭제
/// 4. Discord 스레드 archive + lock
async fn cleanup_orphaned_thread(
    serenity_ctx: &serenity::Context,
    data: &Data,
    thread_channel_id: serenity::ChannelId,
    thread_id: &str,
) {
    // 1. SessionManager에서 세션 kill (best-effort, NotFound 허용)
    if let Err(e) = data.sessions.kill_session(thread_id).await {
        tracing::debug!("cleanup: kill_session {}: {} (may not exist yet)", thread_id, e);
    }

    // 2. 인메모리 tracking 정리
    data.turn_initiators.lock().await.remove(thread_id);
    data.turn_participants.lock().await.remove(thread_id);

    // 3. DB 세션 삭제
    if let Err(e) = repository::delete_session(&data.db, thread_id).await {
        tracing::warn!("cleanup: failed to delete orphan session {}: {}", thread_id, e);
    }

    // 4. Discord 스레드에 경고 메시지 → archive + lock
    let _ = thread_channel_id
        .say(serenity_ctx, "⚠️ 세션 생성에 실패하여 이 스레드는 사용되지 않습니다.")
        .await;
    let _ = thread_channel_id
        .edit_thread(serenity_ctx, EditThread::new().archived(true).locked(true))
        .await;
}

// ── 초기 turn 소비 ──

/// 새 세션의 initial turn 이벤트를 Discord에 출력하지 않고 조용히 소비하면서
/// session_id / skills / last_active / status를 DB에 저장.
async fn drain_initial_turn(
    mut event_rx: mpsc::Receiver<StreamEvent>,
    thread_id: &str,
    db: &sqlx::SqlitePool,
    session_skills: Arc<tokio::sync::Mutex<std::collections::HashMap<String, Vec<String>>>>,
    timeout_secs: u64,
) -> Result<(), PidoryError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        tokio::select! {
            event = event_rx.recv() => {
                match event {
                    Some(StreamEvent::Init { skills, .. }) => {
                        if !skills.is_empty() {
                            session_skills.lock().await.insert(thread_id.to_string(), skills.clone());
                        }
                    }
                    Some(StreamEvent::Result { session_id, is_error, .. }) => {
                        if !session_id.is_empty()
                            && let Err(e) = repository::update_session_id(db, thread_id, &session_id).await
                        {
                            tracing::warn!("drain_initial_turn: failed to update session_id: {}", e);
                        }
                        if is_error {
                            if let Err(e) = repository::update_session_status(db, thread_id, "error").await {
                                tracing::warn!("drain_initial_turn: failed to update status to error: {}", e);
                            }
                            return Err(PidoryError::Subprocess("initial turn error".into()));
                        } else {
                            if let Err(e) = repository::update_session_status(db, thread_id, "idle").await {
                                tracing::warn!("drain_initial_turn: failed to update status to idle: {}", e);
                            }
                        }
                        break;
                    }
                    None => {
                        if let Err(e) = repository::update_session_status(db, thread_id, "error").await {
                            tracing::warn!("drain_initial_turn: failed to update status on channel close: {}", e);
                        }
                        return Err(PidoryError::Subprocess("initial turn channel closed".into()));
                    }
                    Some(StreamEvent::Assistant { ref content, .. }) => {
                        // WARN2: LLM이 프롬프트 무시하고 tool 사용 시도 감지
                        for block in content {
                            if let ContentBlock::ToolUse { name, .. } = block {
                                tracing::warn!(
                                    "drain_initial_turn: unexpected tool_use '{}' during bootstrap for {}",
                                    name, thread_id
                                );
                            }
                        }
                    }
                    Some(StreamEvent::ControlRequest { .. }) => {
                        // WARN1: bootstrap 중 permission 요청 — 프롬프트 무시 가능성
                        tracing::warn!(
                            "drain_initial_turn: unexpected ControlRequest during bootstrap for {}",
                            thread_id
                        );
                    }
                    _ => {} // User, RateLimit 등 무시
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                if let Err(e) = repository::update_session_status(db, thread_id, "error").await {
                    tracing::warn!("drain_initial_turn: failed to update status on timeout: {}", e);
                }
                return Err(PidoryError::Subprocess("initial turn timeout".into()));
            }
        }
    }

    if let Err(e) = repository::update_last_active(db, thread_id).await {
        tracing::warn!("drain_initial_turn: failed to update last_active: {}", e);
    }

    Ok(())
}

// ── 요약 수집 ──

/// 요약 turn의 StreamEvent를 수집. Discord에 출력하지 않고 텍스트만 반환.
async fn collect_summary_response(
    mut event_rx: mpsc::Receiver<StreamEvent>,
    timeout_secs: u64,
) -> Result<String, PidoryError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut text = String::new();

    loop {
        tokio::select! {
            event = event_rx.recv() => {
                match event {
                    Some(StreamEvent::Assistant { ref content, .. }) => {
                        for block in content {
                            if let ContentBlock::Text(t) = block {
                                text.push_str(t);
                            }
                        }
                    }
                    Some(StreamEvent::Result { .. }) => break,
                    None => break,
                    _ => {} // RateLimit, ControlRequest 등 무시
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(PidoryError::Subprocess("branch summary timeout".into()));
            }
        }
    }

    if text.is_empty() {
        return Err(PidoryError::Subprocess("empty summary response".into()));
    }
    Ok(text)
}

// ── 유틸리티 ──

pub(crate) struct BranchSummary {
    pub title: String,
    pub summary: String,
}

/// LLM 응답에서 JSON 파싱. 실패 시 문자열 매칭 fallback.
pub(crate) fn parse_summary_response(text: &str) -> Option<BranchSummary> {
    // 1차: 첫 '{' ~ 마지막 '}' 추출 후 serde_json 파싱
    if let Some(start) = text.find('{')
        && let Some(end) = text.rfind('}')
        && end > start
    {
        let json_str = &text[start..=end];
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
            let title = val.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let summary = val.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            if !title.is_empty() && !summary.is_empty() {
                return Some(BranchSummary {
                    title: title.to_string(),
                    summary: summary.to_string(),
                });
            }
        }
    }

    // 2차: fallback — "title" / "summary" 키를 수동 추출
    let title = extract_quoted_value(text, "title");
    let summary = extract_quoted_value(text, "summary");

    if let (Some(t), Some(s)) = (title, summary)
        && !t.is_empty()
        && !s.is_empty()
    {
        return Some(BranchSummary {
            title: t,
            summary: s,
        });
    }

    None
}

/// `"key": "value"` 패턴에서 value 추출. 이스케이프된 따옴표 처리.
fn extract_quoted_value(text: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let key_pos = text.find(&pattern)?;
    let after_key = &text[key_pos + pattern.len()..];

    let colon_pos = after_key.find(':')?;
    let after_colon = &after_key[colon_pos + 1..];

    let quote_start = after_colon.find('"')?;
    let value_start = &after_colon[quote_start + 1..];

    let mut result = String::new();
    let mut chars = value_start.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                match next {
                    '"' => result.push('"'),
                    '\\' => result.push('\\'),
                    'n' => result.push('\n'),
                    _ => {
                        result.push('\\');
                        result.push(next);
                    }
                }
            }
        } else if ch == '"' {
            break;
        } else {
            result.push(ch);
        }
    }

    Some(result)
}

/// Discord 스레드 이름 sanitize.
pub(crate) fn sanitize_thread_title(title: &str) -> String {
    let mut result = title.replace("@everyone", "").replace("@here", "");
    result = result.replace(['\n', '\r'], "");
    result = result.trim().to_string();

    if result.chars().count() > 100 {
        result = result.chars().take(100).collect();
    }

    if result.is_empty() {
        "Branch".to_string()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_json() {
        let text = r#"{"title": "Auth 모듈 구현", "summary": "Discord 봇 프로젝트에서 인증 모듈을 구현 중"}"#;
        let result = parse_summary_response(text).unwrap();
        assert_eq!(result.title, "Auth 모듈 구현");
        assert_eq!(result.summary, "Discord 봇 프로젝트에서 인증 모듈을 구현 중");
    }

    #[test]
    fn parse_json_with_surrounding_text() {
        let text = r#"Here is the summary: {"title": "Bug fix", "summary": "Fixing a critical bug in session management"} Hope this helps!"#;
        let result = parse_summary_response(text).unwrap();
        assert_eq!(result.title, "Bug fix");
        assert!(result.summary.contains("critical bug"));
    }

    #[test]
    fn parse_fallback_extraction() {
        let text = r#"The "title": "Refactoring plan" and "summary": "We are refactoring the handler module""#;
        let result = parse_summary_response(text).unwrap();
        assert_eq!(result.title, "Refactoring plan");
        assert_eq!(result.summary, "We are refactoring the handler module");
    }

    #[test]
    fn parse_completely_invalid() {
        let text = "This is just random text with no structure at all.";
        assert!(parse_summary_response(text).is_none());
    }

    #[test]
    fn parse_empty_fields_returns_none() {
        let text = r#"{"title": "", "summary": "some content"}"#;
        assert!(parse_summary_response(text).is_none());
    }

    #[test]
    fn sanitize_removes_mentions() {
        assert_eq!(sanitize_thread_title("Hello @everyone world"), "Hello  world");
        assert_eq!(sanitize_thread_title("Test @here end"), "Test  end");
    }

    #[test]
    fn sanitize_removes_newlines() {
        assert_eq!(sanitize_thread_title("Line1\nLine2\rLine3"), "Line1Line2Line3");
    }

    #[test]
    fn sanitize_truncates_to_100() {
        let long_title = "가".repeat(150);
        let result = sanitize_thread_title(&long_title);
        assert_eq!(result.chars().count(), 100);
    }

    #[test]
    fn sanitize_empty_becomes_branch() {
        assert_eq!(sanitize_thread_title(""), "Branch");
        assert_eq!(sanitize_thread_title("   "), "Branch");
    }

    #[test]
    fn sanitize_normal_title() {
        assert_eq!(sanitize_thread_title("Auth 모듈 구현"), "Auth 모듈 구현");
    }
}
