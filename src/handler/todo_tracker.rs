use std::time::{Duration, Instant};

use poise::serenity_prelude::{self as serenity, ChannelId, CreateMessage, EditMessage, MessageFlags, MessageId};
use serde_json::Value;
use reqwest::StatusCode;

const COOLDOWN: Duration = Duration::from_secs(3);

pub struct TodoTracker {
    channel_id: ChannelId,
    message_id: Option<MessageId>,
    last_edit: Option<Instant>,
    pending_input: Option<Value>,
}

impl TodoTracker {
    pub fn new(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            message_id: None,
            last_edit: None,
            pending_input: None,
        }
    }

    pub async fn update(&mut self, ctx: &serenity::Context, input: &Value) {
        // todos 비어있으면 return
        match input.get("todos").and_then(|v| v.as_array()) {
            Some(arr) if !arr.is_empty() => {}
            _ => return,
        }

        let embed = match crate::handler::formatter::format_todo_embed(input) {
            Some(e) => e,
            None => return,
        };

        if self.message_id.is_none() {
            // 신규 메시지 생성
            let msg = CreateMessage::new()
                .embed(embed)
                .flags(MessageFlags::SUPPRESS_NOTIFICATIONS);
            match self.channel_id.send_message(ctx, msg).await {
                Ok(m) => {
                    let mid = m.id;
                    self.message_id = Some(mid);
                    self.last_edit = Some(Instant::now());
                    // pin 시도 — 실패해도 계속 진행
                    if let Err(e) = self.channel_id.pin(ctx, mid).await {
                        tracing::warn!("TodoTracker: failed to pin message {}: {}", mid, e);
                    }
                }
                Err(e) => {
                    tracing::warn!("TodoTracker: failed to create embed message: {}", e);
                }
            }
        } else {
            // 기존 메시지 편집 — cooldown 체크
            let should_update = match self.last_edit {
                None => true,
                Some(last) => last.elapsed() >= COOLDOWN,
            };

            if should_update {
                self.do_edit(ctx, input).await;
            } else {
                // cooldown 중 — latest wins 버퍼링
                self.pending_input = Some(input.clone());
            }
        }
    }

    /// 턴 종료 시 호출 — pending_input이 있으면 cooldown 무시하고 강제 반영.
    pub async fn flush(&mut self, ctx: &serenity::Context) {
        let input = match self.pending_input.take() {
            Some(v) => v,
            None => return,
        };

        let embed = match crate::handler::formatter::format_todo_embed(&input) {
            Some(e) => e,
            None => return,
        };

        if let Some(mid) = self.message_id {
            if let Err(e) = self
                .channel_id
                .edit_message(ctx, mid, EditMessage::new().embed(embed))
                .await
            {
                tracing::warn!("TodoTracker: flush — failed to edit message {}: {}", mid, e);
                // 404 Not Found — message_id 리셋, 재생성은 다음 update() 호출에서 처리
                let is_not_found = matches!(&e,
                    serenity::Error::Http(http_err) if http_err.status_code() == Some(StatusCode::NOT_FOUND)
                );
                if is_not_found {
                    self.message_id = None;
                    self.pending_input = Some(input);
                }
                // transient error: message_id 유지, pending_input은 그대로
            } else {
                self.last_edit = Some(Instant::now());
            }
        } else {
            // message_id가 없으면 일반 update 경로로 처리
            self.update(ctx, &input).await;
        }
    }

    /// 세션 종료 시 호출 — unpin + delete.
    pub async fn cleanup(&mut self, ctx: &serenity::Context) {
        let mid = match self.message_id.take() {
            Some(id) => id,
            None => return,
        };

        if let Err(e) = self.channel_id.unpin(ctx, mid).await {
            tracing::warn!("TodoTracker: failed to unpin message {}: {}", mid, e);
        }

        if let Err(e) = self.channel_id.delete_message(ctx, mid).await {
            tracing::warn!("TodoTracker: failed to delete message {}: {}", mid, e);
        }
    }

    /// 공통 edit 로직 — edit 실패 시 message_id를 None으로 리셋하고 pending에 저장.
    async fn do_edit(&mut self, ctx: &serenity::Context, input: &Value) {
        let embed = match crate::handler::formatter::format_todo_embed(input) {
            Some(e) => e,
            None => return,
        };

        let mid = match self.message_id {
            Some(id) => id,
            None => return,
        };

        if let Err(e) = self
            .channel_id
            .edit_message(ctx, mid, EditMessage::new().embed(embed))
            .await
        {
            tracing::warn!("TodoTracker: failed to edit message {}: {}", mid, e);
            // 404 Not Found — message_id 리셋, 다음 update()에서 재생성
            let is_not_found = matches!(&e,
                serenity::Error::Http(http_err) if http_err.status_code() == Some(StatusCode::NOT_FOUND)
            );
            if is_not_found {
                self.message_id = None;
                self.pending_input = Some(input.clone());
            }
            // transient error: message_id 유지, pending_input은 그대로
        } else {
            self.last_edit = Some(Instant::now());
        }
    }
}
