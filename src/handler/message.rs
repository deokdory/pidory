use std::time::{Duration, Instant};

use poise::serenity_prelude::{Context, FullEvent, GuildId, UserId};
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use crate::db::repository;
use crate::error::PidoryError;
use crate::handler::{emoji, formatter};
use crate::handler::emoji::ReactionStatus;
use crate::handler::status::StatusMessage;
use crate::subprocess::parser::{ContentBlock, StreamEvent};
use crate::Data;

pub async fn handle_event(
    ctx: &Context,
    event: &FullEvent,
    data: &Data,
) -> Result<(), PidoryError> {
    let FullEvent::Message { new_message } = event else {
        return Ok(());
    };

    // bot 자신의 메시지 무시
    if new_message.author.bot {
        return Ok(());
    }

    // guild ID 검증
    if new_message.guild_id != Some(GuildId::new(data.config.discord.guild_id)) {
        return Ok(());
    }

    // owner ID 검증
    if new_message.author.id != UserId::new(data.config.discord.owner_id) {
        return Ok(());
    }

    // 스레드인지 확인
    let channel = match new_message.channel_id.to_channel(ctx).await {
        Ok(ch) => ch,
        Err(e) => {
            warn!("Failed to fetch channel {}: {}", new_message.channel_id, e);
            return Ok(());
        }
    };

    let guild_channel = match channel.guild() {
        Some(gc) => gc,
        None => return Ok(()),
    };

    if guild_channel.thread_metadata.is_none() {
        return Ok(());
    }

    // parent channel ID 추출
    let parent_channel_id = match guild_channel.parent_id {
        Some(pid) => pid.to_string(),
        None => return Ok(()),
    };

    // parent channel에 등록된 프로젝트 확인
    let db = &data.db;
    let project = match repository::get_project_by_channel(db, &parent_channel_id).await? {
        Some(p) => p,
        None => return Ok(()),
    };

    let thread_id = new_message.channel_id.to_string();
    let channel_id = new_message.channel_id;
    let msg_id = new_message.id;

    // 세션 확인/생성
    let session = match repository::get_session_by_thread(db, &thread_id).await? {
        Some(s) => s,
        None => {
            tracing::info!("Creating new session for thread {}", thread_id);
            repository::create_session(db, &thread_id, &parent_channel_id).await?
        }
    };

    // 원자적 acquire: running이 아닌 경우에만 running으로 전환
    let acquired = repository::try_acquire_session(db, &thread_id).await?;
    if !acquired {
        channel_id
            .say(ctx, "⏳ 이전 작업이 진행 중입니다")
            .await
            .map_err(|e| PidoryError::Discord(Box::new(e)))?;
        return Ok(());
    }

    // subprocess 실행 직전에 🔄 리액션 설정
    emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Running)
        .await
        .ok();

    // disallowed_tools 결정
    let disallowed_tools: Vec<String> = match &project.disallowed_tools {
        Some(json_str) => serde_json::from_str(json_str).unwrap_or_else(|e| {
            warn!("Failed to parse disallowed_tools JSON: {}", e);
            data.config.claude.default_disallowed_tools.clone()
        }),
        None => data.config.claude.default_disallowed_tools.clone(),
    };

    tracing::info!(
        "Spawning subprocess for thread {} project {}",
        thread_id, project.path
    );

    let spawn_result = data
        .subprocess
        .spawn(
            &thread_id,
            &project.path,
            &new_message.content,
            session.session_id.as_deref(),
            &disallowed_tools,
        )
        .await;

    match spawn_result {
        Ok((mut rx, _handle)) => {
            // 1. StatusMessage 생성
            let mut status = StatusMessage::new(channel_id);

            // 2. typing indicator task 시작
            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();
            let ctx_clone = ctx.clone();
            let typing_channel = channel_id;
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel_clone.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(8)) => {
                            let _ = typing_channel.broadcast_typing(&ctx_clone).await;
                        }
                    }
                }
            });

            // 3. 500ms 빠른 완료 감지
            let mut events: Vec<StreamEvent> = Vec::new();
            let mut got_result = false;
            let mut fast_complete = false;

            // 500ms 동안 이벤트 수집 (빠른 완료 감지)
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break; // 500ms 경과
                }
                match tokio::time::timeout(remaining, rx.recv()).await {
                    Ok(Some(event)) => {
                        if let StreamEvent::Result { .. } = &event {
                            got_result = true;
                        }
                        events.push(event);
                        if got_result {
                            fast_complete = true;
                            break; // Result 수신 → 빠른 완료
                        }
                    }
                    Ok(None) => {
                        // sender dropped (프로세스 종료)
                        fast_complete = true;
                        break;
                    }
                    Err(_) => {
                        break; // 500ms timeout
                    }
                }
            }

            // 4. 빠른 완료가 아니면 상태 메시지 + 나머지 이벤트 루프
            if !fast_complete {
                // 버퍼링된 이벤트에 대해 상태 업데이트
                for event in &events {
                    status.update(ctx, event).await.ok();
                }

                // 나머지 이벤트 계속 수신
                while let Some(stream_event) = rx.recv().await {
                    status.update(ctx, &stream_event).await.ok();
                    if let StreamEvent::Result { .. } = &stream_event {
                        got_result = true;
                    }
                    events.push(stream_event);
                }
            }

            // 5. typing indicator 취소
            cancel.cancel();

            // session_id 추출
            for event in &events {
                if let StreamEvent::Result { session_id, .. } = event
                    && !session_id.is_empty()
                {
                    if let Err(e) = repository::update_session_id(db, &thread_id, session_id).await {
                        warn!("Failed to update session_id: {}", e);
                    }
                    break;
                }
            }

            // 6. 에러 체크
            let has_cli_error = events.iter().any(|e| e.is_error());

            // 7. 상태 메시지 최종 처리
            if fast_complete {
                // 빠른 완료: 상태 메시지가 생성되지 않았으므로 finalize/set_error 호출 안 함
                if has_cli_error || !got_result {
                    emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error).await.ok();
                    repository::update_session_status(db, &thread_id, "error").await?;
                } else {
                    emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Done).await.ok();
                    repository::update_session_status(db, &thread_id, "idle").await?;
                }
            } else {
                // 마지막 debounce 반영
                status.flush(ctx).await.ok();

                if has_cli_error {
                    let error_msgs: Vec<String> = events.iter().filter_map(|e| {
                        if let StreamEvent::Result { is_error, errors, .. } = e {
                            if *is_error { Some(errors.join(", ")) } else { None }
                        } else {
                            None
                        }
                    }).collect();
                    let error_text = error_msgs.first().cloned().unwrap_or_else(|| "unknown error".to_string());
                    status.set_error(ctx, &error_text).await.ok();
                    emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error).await.ok();
                    repository::update_session_status(db, &thread_id, "error").await?;
                } else if !got_result {
                    // Result 없이 종료 = crash 또는 timeout
                    status.set_error(ctx, "프로세스가 비정상 종료되었습니다").await.ok();
                    emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Error).await.ok();
                    repository::update_session_status(db, &thread_id, "error").await?;
                } else {
                    // 성공 — 상태 메시지 삭제
                    status.finalize(ctx).await.ok();
                    emoji::set_reaction(ctx, channel_id, msg_id, ReactionStatus::Done).await.ok();
                    repository::update_session_status(db, &thread_id, "idle").await?;
                }
            }

            repository::update_last_active(db, &thread_id).await?;

            // 8. 최종 응답 전송
            let response = formatter::format_response(&events);
            if !response.trim().is_empty() {
                formatter::send_response(
                    ctx,
                    channel_id,
                    &response,
                    data.config.response.max_chunk_length,
                    data.config.response.max_chunks,
                )
                .await?;
            }
        }
        Err(e) => {
            // spawn 자체 실패 (concurrent limit 등)
            error!("Subprocess error for thread {}: {}", thread_id, e);
            let (reaction, error_msg) = classify_error(&e, data.config.claude.subprocess_timeout_secs);
            emoji::set_reaction(ctx, channel_id, msg_id, reaction).await.ok();
            repository::update_session_status(db, &thread_id, "error").await?;
            channel_id
                .say(ctx, error_msg)
                .await
                .map_err(|e| PidoryError::Discord(Box::new(e)))?;
        }
    }

    Ok(())
}

fn classify_error(error: &PidoryError, timeout_secs: u64) -> (ReactionStatus, String) {
    match error {
        PidoryError::Subprocess(msg) if msg == "timeout" => (
            ReactionStatus::Timeout,
            format!("⏰ 작업 시간이 초과되었습니다 ({}분)", timeout_secs / 60),
        ),
        PidoryError::Subprocess(msg) if msg.contains("max concurrent") => (
            ReactionStatus::Error,
            "⚠️ 동시 실행 상한에 도달했습니다. 잠시 후 다시 시도해주세요.".to_string(),
        ),
        PidoryError::Subprocess(msg) => (
            ReactionStatus::Error,
            format!("❌ Claude 프로세스 오류: {}", msg),
        ),
        _ => (
            ReactionStatus::Error,
            format!("❌ 오류가 발생했습니다: {}", error),
        ),
    }
}
