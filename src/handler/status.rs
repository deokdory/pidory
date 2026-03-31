use std::time::{Duration, Instant};

use poise::serenity_prelude::{self as serenity, ChannelId, MessageId};

use crate::error::PidoryError;
use crate::subprocess::parser::{ContentBlock, StreamEvent};

pub struct StatusMessage {
    channel_id: ChannelId,
    message_id: Option<MessageId>,
    pending_text: String,
    last_edit: Instant,
    tool_history: Vec<String>,
    needs_update: bool,
}

impl StatusMessage {
    pub fn new(channel_id: ChannelId) -> Self {
        Self {
            channel_id,
            message_id: None,
            pending_text: String::new(),
            last_edit: Instant::now(),
            tool_history: Vec::new(),
            needs_update: false,
        }
    }

    pub async fn update(
        &mut self,
        ctx: &serenity::Context,
        event: &StreamEvent,
    ) -> Result<(), PidoryError> {
        if let StreamEvent::Assistant { content, .. } = event {
            for block in content {
                if let ContentBlock::ToolUse { name, input, .. } = block {
                    let entry = format_tool_entry(name, input);
                    self.tool_history.push(entry);
                }
            }
            self.rebuild_text();
            self.try_send(ctx).await?;
        }
        Ok(())
    }

    fn rebuild_text(&mut self) {
        loop {
            let text = build_text(&self.tool_history);
            if text.len() <= 2000 {
                self.pending_text = text;
                self.needs_update = true;
                break;
            }
            if self.tool_history.is_empty() {
                self.pending_text = "⏳ 작업 중...".to_string();
                self.needs_update = true;
                break;
            }
            self.tool_history.remove(0);
        }
    }

    pub async fn try_send(&mut self, ctx: &serenity::Context) -> Result<(), PidoryError> {
        if !self.needs_update {
            return Ok(());
        }
        if self.last_edit.elapsed() < Duration::from_millis(1500) {
            return Ok(());
        }
        self.send_now(ctx).await;
        Ok(())
    }

    async fn send_now(&mut self, ctx: &serenity::Context) {
        let result = if let Some(mid) = self.message_id {
            self.channel_id
                .edit_message(ctx, mid, serenity::EditMessage::new().content(&self.pending_text))
                .await
                .map(|_| None)
        } else {
            self.channel_id
                .say(ctx, &self.pending_text)
                .await
                .map(|msg| Some(msg.id))
        };

        match result {
            Ok(Some(id)) => {
                self.message_id = Some(id);
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("StatusMessage send_now failed: {}", e);
                return;
            }
        }

        self.last_edit = Instant::now();
        self.needs_update = false;
    }

    pub async fn flush(&mut self, ctx: &serenity::Context) -> Result<(), PidoryError> {
        if self.needs_update {
            self.send_now(ctx).await;
        }
        Ok(())
    }

    pub async fn finalize(&mut self, ctx: &serenity::Context) -> Result<(), PidoryError> {
        if let Some(mid) = self.message_id.take() {
            let _ = self.channel_id.delete_message(ctx, mid).await;
        }
        Ok(())
    }

    pub async fn set_error(
        &mut self,
        ctx: &serenity::Context,
        error: &str,
    ) -> Result<(), PidoryError> {
        let text = format!("❌ 오류 — {}", error);
        self.pending_text = text;
        self.needs_update = true;
        self.last_edit = Instant::now() - Duration::from_secs(60);
        self.send_now(ctx).await;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn has_message(&self) -> bool {
        self.message_id.is_some()
    }
}

fn format_tool_entry(name: &str, input: &serde_json::Value) -> String {
    match name {
        "Bash" => {
            let command = input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let short = if command.len() > 50 {
                format!("{}...", &command[..50])
            } else {
                command.to_string()
            };
            format!("🔧 Bash: `{}`", short)
        }
        "Edit" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("📝 Edit: {}", file_path)
        }
        "Read" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("🔍 Read: {}", file_path)
        }
        "Write" => {
            let file_path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("📄 Write: {}", file_path)
        }
        "Grep" => {
            let pattern = input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("🔎 Grep: {}", pattern)
        }
        "Glob" => {
            let pattern = input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("📁 Glob: {}", pattern)
        }
        "Agent" => "🤖 Agent".to_string(),
        _ => format!("🔧 {}", name),
    }
}

fn build_text(tool_history: &[String]) -> String {
    let mut lines = vec!["⏳ 작업 중...".to_string()];

    const MAX_SHOWN: usize = 5;

    if tool_history.len() > MAX_SHOWN {
        let hidden = tool_history.len() - MAX_SHOWN;
        lines.push(format!("... +{} more", hidden));
        for entry in &tool_history[tool_history.len() - MAX_SHOWN..] {
            lines.push(entry.clone());
        }
    } else {
        for entry in tool_history {
            lines.push(entry.clone());
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_text_empty() {
        let text = build_text(&[]);
        assert_eq!(text, "⏳ 작업 중...");
    }

    #[test]
    fn build_text_few_tools() {
        let tools = vec![
            "🔧 Bash: `echo hello`".to_string(),
            "📝 Edit: src/main.rs".to_string(),
        ];
        let text = build_text(&tools);
        assert!(text.contains("⏳ 작업 중..."));
        assert!(text.contains("🔧 Bash"));
        assert!(text.contains("📝 Edit"));
    }

    #[test]
    fn build_text_overflow_shows_recent() {
        let tools: Vec<String> = (0..7).map(|i| format!("🔧 Tool{}", i)).collect();
        let text = build_text(&tools);
        assert!(text.contains("... +2 more"));
        assert!(text.contains("Tool2")); // 최근 5개
        assert!(text.contains("Tool6"));
        assert!(!text.contains("Tool0")); // 오래된 것은 안 보임
        assert!(!text.contains("Tool1"));
    }

    #[test]
    fn build_text_under_2000_chars() {
        let tools: Vec<String> = (0..100).map(|i| format!("🔧 Tool {}", i)).collect();
        let text = build_text(&tools);
        assert!(text.len() <= 2000);
    }

    #[test]
    fn format_tool_entry_bash() {
        let input = serde_json::json!({"command": "echo hello world"});
        let entry = format_tool_entry("Bash", &input);
        assert!(entry.contains("🔧 Bash"));
        assert!(entry.contains("echo hello world"));
    }

    #[test]
    fn format_tool_entry_bash_truncate() {
        let long_cmd = "x".repeat(100);
        let input = serde_json::json!({"command": long_cmd});
        let entry = format_tool_entry("Bash", &input);
        assert!(entry.contains("..."));
        assert!(entry.len() < 70); // 50 + emoji + prefix
    }

    #[test]
    fn format_tool_entry_edit() {
        let input = serde_json::json!({"file_path": "/tmp/foo.rs"});
        let entry = format_tool_entry("Edit", &input);
        assert_eq!(entry, "📝 Edit: /tmp/foo.rs");
    }

    #[test]
    fn format_tool_entry_read() {
        let input = serde_json::json!({"file_path": "/tmp/bar.rs"});
        let entry = format_tool_entry("Read", &input);
        assert_eq!(entry, "🔍 Read: /tmp/bar.rs");
    }

    #[test]
    fn format_tool_entry_unknown() {
        let input = serde_json::json!({});
        let entry = format_tool_entry("CustomTool", &input);
        assert_eq!(entry, "🔧 CustomTool");
    }

    #[test]
    fn format_tool_entry_agent() {
        let input = serde_json::json!({});
        let entry = format_tool_entry("Agent", &input);
        assert_eq!(entry, "🤖 Agent");
    }

    #[test]
    fn status_message_initial_state() {
        use poise::serenity_prelude::ChannelId;
        let status = StatusMessage::new(ChannelId::new(123));
        assert!(!status.has_message());
    }
}
