//! ConflictNotifier trait and default LoggingNotifier for merge outcome callbacks.
//!
//! # P1.2 흡수 방어 (Metis S3.2)
//!
//! `LoggingNotifier`는 P1.2 (#287)가 `DiscordNotifier`를 추가할 때까지의 default 구현체다.
//! 이 모듈에서 Discord 통합 추가 X. Discord 통합은 전적으로 P1.2 책임.
//!
//! # 구조
//!
//! - [`ConflictNotifier`] — sync trait, P1.2가 `DiscordNotifier`를 impl 가능하도록 object-safe
//! - [`ConflictEvent`] — 4가지 충돌 시나리오 (mtime 변경, lock 타임아웃, JSON 손상, fsync 실패)
//! - [`LoggingNotifier`] — `tracing::warn!` 호출만 하는 default 구현체
//! - [`MergeOutcome`] — T9 public API에서 MergeAction → MergeOutcome 변환에 사용

use std::path::PathBuf;
use std::time::Duration;

/// 충돌/이상 상황 이벤트. T8 RMW core에서 생성하고 ConflictNotifier에 전달한다.
#[derive(Debug)]
pub enum ConflictEvent {
    /// mtime이 바뀐 채 발견됨 — 외부 편집 의심
    MtimeChanged {
        path: PathBuf,
        attempted_rule: String,
    },
    /// advisory lock 획득 대기 중 타임아웃
    LockTimeout { path: PathBuf, waited: Duration },
    /// JSON 파싱 실패 — 백업에서 복구 시도
    JsonCorrupted {
        path: PathBuf,
        backup: PathBuf,
        reason: String,
    },
    /// fsync 실패 — 디스크 쓰기 보장 불가
    FsyncFailed { path: PathBuf, source: String },
}

/// 충돌/이상 상황 알림 trait.
///
/// sync 호출 가능 (async X). `notify_conflict`는 Result를 반환하지 않는다 — 로깅/알림 실패가
/// RMW 흐름을 중단시켜서는 안 되므로.
///
/// P1.2 (#287)에서 `DiscordNotifier`가 이 trait을 impl한다.
pub trait ConflictNotifier: Send + Sync {
    fn notify_conflict(&self, event: ConflictEvent);
}

/// `tracing::warn!` 호출만 하는 default 구현체.
///
/// P1.2 (#287)가 `DiscordNotifier`를 추가할 때까지 default로 사용된다.
pub struct LoggingNotifier;

impl ConflictNotifier for LoggingNotifier {
    fn notify_conflict(&self, event: ConflictEvent) {
        match event {
            ConflictEvent::MtimeChanged {
                ref path,
                ref attempted_rule,
            } => {
                tracing::warn!(
                    path = %path.display(),
                    attempted_rule = %attempted_rule,
                    "ConflictEvent::MtimeChanged — 외부 편집 감지, rule 추가 중단"
                );
            }
            ConflictEvent::LockTimeout {
                ref path,
                ref waited,
            } => {
                tracing::warn!(
                    path = %path.display(),
                    waited_ms = %waited.as_millis(),
                    "ConflictEvent::LockTimeout — advisory lock 획득 실패"
                );
            }
            ConflictEvent::JsonCorrupted {
                ref path,
                ref backup,
                ref reason,
            } => {
                tracing::warn!(
                    path = %path.display(),
                    backup = %backup.display(),
                    reason = %reason,
                    "ConflictEvent::JsonCorrupted — JSON 손상, 백업에서 복구 시도"
                );
            }
            ConflictEvent::FsyncFailed {
                ref path,
                ref source,
            } => {
                tracing::warn!(
                    path = %path.display(),
                    source = %source,
                    "ConflictEvent::FsyncFailed — fsync 실패, 디스크 쓰기 보장 불가"
                );
            }
        }
    }
}

/// RMW 작업 결과. T9 public API에서 내부 MergeAction → MergeOutcome 변환에 사용된다.
#[derive(Debug, PartialEq)]
pub enum MergeOutcome {
    /// 규칙이 새로 추가됨
    Added,
    /// 규칙이 이미 존재해 추가 생략
    AlreadyPresent,
    /// 충돌이 있었으나 자동 merge 성공
    ConflictResolved,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_notifier_handles_all_variants_without_panic() {
        let notifier = LoggingNotifier;

        notifier.notify_conflict(ConflictEvent::MtimeChanged {
            path: PathBuf::from("/tmp/settings.json"),
            attempted_rule: "Bash(npm *)".to_string(),
        });

        notifier.notify_conflict(ConflictEvent::LockTimeout {
            path: PathBuf::from("/tmp/settings.json"),
            waited: Duration::from_millis(5000),
        });

        notifier.notify_conflict(ConflictEvent::JsonCorrupted {
            path: PathBuf::from("/tmp/settings.json"),
            backup: PathBuf::from("/tmp/settings.json.bak"),
            reason: "unexpected EOF".to_string(),
        });

        notifier.notify_conflict(ConflictEvent::FsyncFailed {
            path: PathBuf::from("/tmp/settings.json"),
            source: "Input/output error (os error 5)".to_string(),
        });
    }

    #[test]
    fn external_impl_possible_simulating_p1_2_discord_notifier() {
        struct TestNotifier {
            events: std::sync::Mutex<Vec<String>>,
        }

        impl ConflictNotifier for TestNotifier {
            fn notify_conflict(&self, e: ConflictEvent) {
                let label = match &e {
                    ConflictEvent::MtimeChanged { .. } => "MtimeChanged",
                    ConflictEvent::LockTimeout { .. } => "LockTimeout",
                    ConflictEvent::JsonCorrupted { .. } => "JsonCorrupted",
                    ConflictEvent::FsyncFailed { .. } => "FsyncFailed",
                };
                self.events.lock().unwrap().push(label.to_string());
            }
        }

        let notifier = TestNotifier {
            events: std::sync::Mutex::new(Vec::new()),
        };

        notifier.notify_conflict(ConflictEvent::MtimeChanged {
            path: PathBuf::from("/tmp/settings.json"),
            attempted_rule: "Bash(git *)".to_string(),
        });
        notifier.notify_conflict(ConflictEvent::LockTimeout {
            path: PathBuf::from("/tmp/settings.json"),
            waited: Duration::from_secs(3),
        });

        let events = notifier.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], "MtimeChanged");
        assert_eq!(events[1], "LockTimeout");

        // trait object-safe 검증 — dyn ConflictNotifier로 사용 가능해야 함
        let _dyn_ref: &dyn ConflictNotifier = &notifier;
    }
}
