//! Discord ConflictNotifier impl for atomic editor (P1.1).
//!
//! `ConflictNotifier`는 sync trait이지만 Discord API 호출은 async.
//! → `tokio::spawn`으로 발사 후 잊기. spawn 안에서 `tracing::warn!`으로 silent fail 방지.

use poise::serenity_prelude::{
    ComponentInteraction, Context, CreateInteractionResponseFollowup,
};

use crate::claude_settings::{ConflictEvent, ConflictNotifier};
use crate::i18n::Lang;

#[allow(dead_code)]
pub struct DiscordNotifier {
    pub ctx: Context,
    pub interaction: ComponentInteraction,
    pub lang: Lang,
}

impl ConflictNotifier for DiscordNotifier {
    fn notify_conflict(&self, event: ConflictEvent) {
        let ctx = self.ctx.clone();
        let interaction = self.interaction.clone();
        let lang = self.lang;
        tokio::spawn(async move {
            let (title, body) = format_event(&event, lang);
            let result = interaction
                .create_followup(
                    &ctx,
                    CreateInteractionResponseFollowup::new()
                        .content(format!("**{}**\n{}", title, body))
                        .ephemeral(true),
                )
                .await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "DiscordNotifier ephemeral 전송 실패");
            }
        });
    }
}

/// 4 ConflictEvent → (title, body) 변환. 한/영 분기.
#[allow(dead_code)]
fn format_event(event: &ConflictEvent, lang: Lang) -> (String, String) {
    match event {
        ConflictEvent::MtimeChanged {
            path,
            attempted_rule,
        } => match lang {
            Lang::Ko => (
                "외부 편집 감지".to_string(),
                format!(
                    "`{}`에서 외부 편집 감지 — rule '{}' 추가 중단",
                    path.display(),
                    attempted_rule
                ),
            ),
            Lang::En => (
                "External edit detected".to_string(),
                format!(
                    "External edit at `{}` — aborted adding rule '{}'",
                    path.display(),
                    attempted_rule
                ),
            ),
        },
        ConflictEvent::LockTimeout { path, waited } => match lang {
            Lang::Ko => (
                "lock 획득 실패".to_string(),
                format!(
                    "`{}` lock timeout ({}ms) — 잠시 후 재시도",
                    path.display(),
                    waited.as_millis()
                ),
            ),
            Lang::En => (
                "Lock acquisition failed".to_string(),
                format!(
                    "`{}` lock timeout ({}ms) — please retry",
                    path.display(),
                    waited.as_millis()
                ),
            ),
        },
        ConflictEvent::JsonCorrupted {
            path,
            backup,
            reason,
        } => match lang {
            Lang::Ko => (
                "settings.json 손상".to_string(),
                format!(
                    "`{}` 손상 — 백업 생성됨 `{}` ({})",
                    path.display(),
                    backup.display(),
                    reason
                ),
            ),
            Lang::En => (
                "settings.json corrupted".to_string(),
                format!(
                    "`{}` corrupted — backup created at `{}` ({})",
                    path.display(),
                    backup.display(),
                    reason
                ),
            ),
        },
        ConflictEvent::FsyncFailed { path, source } => match lang {
            Lang::Ko => (
                "디스크 쓰기 실패".to_string(),
                format!(
                    "`{}` fsync 실패 — 디스크 상태 확인 필요 ({})",
                    path.display(),
                    source
                ),
            ),
            Lang::En => (
                "Disk write failed".to_string(),
                format!(
                    "`{}` fsync failed — check disk health ({})",
                    path.display(),
                    source
                ),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn format_mtime_changed_korean() {
        let event = ConflictEvent::MtimeChanged {
            path: PathBuf::from("/tmp/settings.json"),
            attempted_rule: "Bash(npm *)".to_string(),
        };
        let (title, body) = format_event(&event, Lang::Ko);
        assert_eq!(title, "외부 편집 감지");
        assert!(body.contains("/tmp/settings.json"));
        assert!(body.contains("Bash(npm *)"));
    }

    #[test]
    fn format_lock_timeout_english() {
        let event = ConflictEvent::LockTimeout {
            path: PathBuf::from("/tmp/settings.json"),
            waited: Duration::from_millis(5000),
        };
        let (title, body) = format_event(&event, Lang::En);
        assert_eq!(title, "Lock acquisition failed");
        assert!(body.contains("5000ms"));
    }

    #[test]
    fn format_json_corrupted_korean() {
        let event = ConflictEvent::JsonCorrupted {
            path: PathBuf::from("/tmp/settings.json"),
            backup: PathBuf::from("/tmp/settings.json.corrupted-1234"),
            reason: "expected `}`".to_string(),
        };
        let (title, body) = format_event(&event, Lang::Ko);
        assert_eq!(title, "settings.json 손상");
        assert!(body.contains("백업 생성됨"));
    }

    #[test]
    fn format_fsync_failed_english() {
        let event = ConflictEvent::FsyncFailed {
            path: PathBuf::from("/tmp/settings.json"),
            source: "I/O error".to_string(),
        };
        let (title, body) = format_event(&event, Lang::En);
        assert_eq!(title, "Disk write failed");
        assert!(body.contains("I/O error"));
    }
}
