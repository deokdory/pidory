use std::time::{Duration, Instant};

use poise::serenity_prelude::{self as serenity, ChannelId, CreateMessage, EditMessage, MessageFlags, MessageId};

use crate::i18n::Lang;

pub enum ProgressKind {
    Thinking,
    Tool(String),
}

pub struct ProgressIndicator {
    channel_id: ChannelId,
    message_id: Option<MessageId>,
    kind: ProgressKind,
    started: Instant,
    last_edit: Option<Instant>,
    paused: bool,
    pause_start: Option<Instant>,
    pause_elapsed: Duration,
    active: bool,
    lang: Lang,
    edit_fail_count: u32,
}

impl ProgressIndicator {
    pub fn new(channel_id: ChannelId, lang: Lang) -> Self {
        Self {
            channel_id,
            message_id: None,
            kind: ProgressKind::Thinking,
            started: Instant::now(),
            last_edit: None,
            paused: false,
            pause_start: None,
            pause_elapsed: Duration::ZERO,
            active: false,
            lang,
            edit_fail_count: 0,
        }
    }

    pub async fn on_tool_use(&mut self, name: &str, ctx: &serenity::Context) {
        self.kind = ProgressKind::Tool(name.to_string());
        self.started = Instant::now();
        self.pause_elapsed = Duration::ZERO;
        if self.active {
            self.edit_now(ctx).await;
        }
    }

    pub async fn on_tool_result(&mut self, ctx: &serenity::Context) {
        self.kind = ProgressKind::Thinking;
        self.started = Instant::now();
        self.pause_elapsed = Duration::ZERO;
        if self.active {
            self.edit_now(ctx).await;
        }
    }

    pub fn on_event(&mut self) {
        if let ProgressKind::Thinking = &self.kind {
            self.started = Instant::now();
            self.pause_elapsed = Duration::ZERO;
        }
        // Tool 상태에서는 no-op — tool timer는 on_tool_result에서만 처리
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn on_control_request(&mut self) {
        self.paused = true;
        self.pause_start = Some(Instant::now());
    }

    pub fn on_resume(&mut self) {
        self.paused = false;
        if let Some(start) = self.pause_start.take() {
            self.pause_elapsed += Instant::now() - start;
        }
    }

    async fn edit_now(&mut self, ctx: &serenity::Context) {
        if let Some(mid) = self.message_id {
            let elapsed = (Instant::now() - self.started).saturating_sub(self.pause_elapsed);
            let text = self.format_in_progress(elapsed);
            if let Err(e) = self
                .channel_id
                .edit_message(ctx, mid, EditMessage::new().content(&text))
                .await
            {
                tracing::warn!("ProgressIndicator: failed to edit message: {}", e);
                self.edit_fail_count += 1;
                if self.edit_fail_count >= 3 {
                    tracing::warn!(
                        "ProgressIndicator: deactivating after {} consecutive edit failures",
                        self.edit_fail_count
                    );
                    self.active = false;
                    self.message_id = None;
                }
            } else {
                self.last_edit = Some(Instant::now());
                self.edit_fail_count = 0;
            }
        }
    }

    pub async fn tick(&mut self, ctx: &serenity::Context) {
        if self.paused {
            return;
        }

        let now = Instant::now();
        let elapsed = (now - self.started).saturating_sub(self.pause_elapsed);

        if !self.active {
            // 첫 메시지 생성: 15초 임계값 적용
            if elapsed.as_secs() < 15 {
                return;
            }
            let text = self.format_in_progress(elapsed);
            let msg = CreateMessage::new()
                .content(&text)
                .flags(MessageFlags::SUPPRESS_NOTIFICATIONS);
            match self.channel_id.send_message(ctx, msg).await {
                Ok(m) => {
                    self.message_id = Some(m.id);
                    self.active = true;
                    self.last_edit = Some(now);
                }
                Err(e) => {
                    tracing::warn!("ProgressIndicator: failed to create message: {}", e);
                }
            }
        } else if let Some(last) = self.last_edit {
            // 기존 메시지 갱신: 10초 간격
            if now - last >= Duration::from_secs(10) {
                self.edit_now(ctx).await;
            }
        }
    }

    pub async fn finalize(&mut self, ctx: &serenity::Context) {
        if !self.active {
            return;
        }

        let elapsed = (Instant::now() - self.started).saturating_sub(self.pause_elapsed);
        let text = self.format_done(elapsed);

        if let Some(mid) = self.message_id {
            if let Err(e) = self
                .channel_id
                .edit_message(ctx, mid, EditMessage::new().content(&text))
                .await
            {
                tracing::warn!("ProgressIndicator: failed to finalize message: {}", e);
            }
        }

        self.active = false;
        self.message_id = None;
    }

    pub async fn cleanup(&mut self, ctx: &serenity::Context) {
        if self.active {
            self.finalize(ctx).await;
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    fn format_in_progress(&self, elapsed: Duration) -> String {
        let elapsed_str = format_elapsed(elapsed.as_secs());
        let content = match &self.kind {
            ProgressKind::Tool(name) => self.lang.progress_tool(name, &elapsed_str),
            ProgressKind::Thinking => self.lang.progress_thinking(&elapsed_str),
        };
        format!("-# {}", content)
    }

    fn format_done(&self, elapsed: Duration) -> String {
        let elapsed_str = format_elapsed(elapsed.as_secs());
        let content = match &self.kind {
            ProgressKind::Tool(name) => self.lang.progress_tool_done(name, &elapsed_str),
            ProgressKind::Thinking => self.lang.progress_thinking_done(&elapsed_str),
        };
        format!("-# {}", content)
    }
}

pub fn format_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        format!("{}s", seconds)
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use poise::serenity_prelude::ChannelId;

    use super::{ProgressIndicator, ProgressKind, format_elapsed};
    use crate::i18n::Lang;

    // --- format_elapsed tests ---

    #[test]
    fn format_elapsed_seconds() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(15), "15s");
        assert_eq!(format_elapsed(45), "45s");
        assert_eq!(format_elapsed(59), "59s");
    }

    #[test]
    fn format_elapsed_minutes() {
        assert_eq!(format_elapsed(60), "1m 0s");
        assert_eq!(format_elapsed(125), "2m 5s");
        assert_eq!(format_elapsed(3661), "61m 1s");
    }

    // --- ProgressIndicator state tests ---

    #[test]
    fn new_initial_state() {
        let p = ProgressIndicator::new(ChannelId::new(1), Lang::Ko);
        assert!(!p.is_active());
        assert!(!p.is_paused());
        assert!(matches!(p.kind, ProgressKind::Thinking));
    }

    #[test]
    fn on_control_request_pauses() {
        let mut p = ProgressIndicator::new(ChannelId::new(1), Lang::Ko);
        p.on_control_request();
        assert!(p.is_paused());
    }

    #[test]
    fn on_resume_unpauses() {
        let mut p = ProgressIndicator::new(ChannelId::new(1), Lang::Ko);
        p.on_control_request();
        p.on_resume();
        assert!(!p.is_paused());
    }

    // --- format_in_progress tests ---

    #[test]
    fn format_in_progress_tool() {
        let mut p = ProgressIndicator::new(ChannelId::new(1), Lang::Ko);
        p.kind = ProgressKind::Tool("Bash".to_string());
        let text = p.format_in_progress(Duration::from_secs(30));
        assert_eq!(text, "-# ⏱️ Bash (30s)");
    }

    #[test]
    fn format_in_progress_thinking() {
        let p = ProgressIndicator::new(ChannelId::new(1), Lang::Ko);
        let text = p.format_in_progress(Duration::from_secs(15));
        assert_eq!(text, "-# ⏱️ thinking... (15s)");
    }

    // --- format_done tests ---

    #[test]
    fn format_done_tool() {
        let mut p = ProgressIndicator::new(ChannelId::new(1), Lang::Ko);
        p.kind = ProgressKind::Tool("Bash".to_string());
        let text = p.format_done(Duration::from_secs(45));
        assert_eq!(text, "-# ⏱️ Bash — 45s");
    }

    #[test]
    fn format_done_thinking() {
        let p = ProgressIndicator::new(ChannelId::new(1), Lang::Ko);
        let text = p.format_done(Duration::from_secs(20));
        assert_eq!(text, "-# ⏱️ thinking — 20s");
    }

    #[test]
    fn format_done_with_minutes() {
        let mut p = ProgressIndicator::new(ChannelId::new(1), Lang::Ko);
        p.kind = ProgressKind::Tool("Agent".to_string());
        let text = p.format_done(Duration::from_secs(125));
        assert_eq!(text, "-# ⏱️ Agent — 2m 5s");
    }

    // --- pause_elapsed accumulation test ---

    #[test]
    fn pause_elapsed_accumulates() {
        let mut p = ProgressIndicator::new(ChannelId::new(1), Lang::Ko);
        assert_eq!(p.pause_elapsed, Duration::ZERO);
        p.on_control_request();
        std::thread::sleep(Duration::from_millis(10));
        p.on_resume();
        assert!(p.pause_elapsed > Duration::ZERO);
    }
}
