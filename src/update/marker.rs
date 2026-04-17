use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::Error;

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Marker {
    pending_version: String,
    previous_version: String,
    started_at: u64,
    attempts: u32,
}

#[derive(Debug)]
pub enum RecoveryAction {
    Normal,
    Rolling {
        from: String,
        to: String,
        attempt: u32,
    },
}

fn marker_path(worktree: &Path) -> PathBuf {
    worktree.join("target").join("release").join(".update-pending")
}

pub fn create_marker(
    worktree: &std::path::Path,
    prev_version: &str,
    new_version: &str,
) -> Result<(), Error> {
    let dir = worktree.join("target").join("release");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::BackupFailed(format!("create_dir_all failed: {e}")))?;

    let started_at = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::BackupFailed(format!("system time error: {e}")))?
        .as_secs();

    let marker = Marker {
        pending_version: new_version.to_string(),
        previous_version: prev_version.to_string(),
        started_at,
        attempts: 0,
    };

    let json = serde_json::to_string(&marker)
        .map_err(|e| Error::BackupFailed(format!("json serialize failed: {e}")))?;

    let path = marker_path(worktree);
    std::fs::write(&path, json)
        .map_err(|e| Error::BackupFailed(format!("write marker failed: {e}")))?;

    Ok(())
}

pub fn check_and_recover(worktree: &std::path::Path) -> RecoveryAction {
    let path = marker_path(worktree);

    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return RecoveryAction::Normal,
    };

    let mut marker: Marker = match serde_json::from_str(&contents) {
        Ok(m) => m,
        Err(_) => {
            tracing::warn!("update marker is corrupt, removing and continuing normally");
            let _ = std::fs::remove_file(&path);
            return RecoveryAction::Normal;
        }
    };

    if marker.attempts >= 2 {
        tracing::warn!(
            attempts = marker.attempts,
            "update marker has too many attempts, giving up rollback to avoid infinite loop"
        );
        let _ = std::fs::remove_file(&path);
        return RecoveryAction::Normal;
    }

    // Increment attempt count and persist before returning Rolling.
    marker.attempts += 1;
    let new_attempt = marker.attempts;
    let from = marker.previous_version.clone();
    let to = marker.pending_version.clone();

    if let Ok(json) = serde_json::to_string(&marker) {
        let _ = std::fs::write(&path, json);
    }

    RecoveryAction::Rolling {
        from,
        to,
        attempt: new_attempt,
    }
}

pub fn confirm_ready(worktree: &std::path::Path) -> Result<(), Error> {
    let path = marker_path(worktree);
    // Missing marker is fine — treat as already confirmed.
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| Error::BackupFailed(format!("remove marker failed: {e}")))?;
    }
    Ok(())
}

pub async fn ready_watchdog(
    worktree: std::path::PathBuf,
    ready_rx: tokio::sync::oneshot::Receiver<()>,
) {
    match tokio::time::timeout(Duration::from_secs(60), ready_rx).await {
        Ok(Ok(())) => {
            // Ready signal received — confirm successful boot.
            if let Err(e) = confirm_ready(&worktree) {
                tracing::error!("confirm_ready failed: {e}");
            }
        }
        Ok(Err(_)) => {
            // Sender was dropped without sending — unexpected shutdown.
            tracing::error!(
                "ready signal sender dropped without signaling, exiting for systemd to retry"
            );
            std::process::exit(1);
        }
        Err(_) => {
            // 60-second timeout — bot never became ready.
            tracing::error!("ready signal timed out, exiting for systemd to retry");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_marker_creates_file_with_correct_fields() {
        let dir = tempfile::tempdir().unwrap();
        create_marker(dir.path(), "0.6.2", "0.6.3").unwrap();

        let path = marker_path(dir.path());
        assert!(path.exists(), "marker file should exist after create_marker");

        let contents = std::fs::read_to_string(&path).unwrap();
        let marker: Marker = serde_json::from_str(&contents).unwrap();

        assert_eq!(marker.previous_version, "0.6.2");
        assert_eq!(marker.pending_version, "0.6.3");
        assert_eq!(marker.attempts, 0);
        assert!(marker.started_at > 0);
    }

    #[test]
    fn test_check_and_recover_no_marker_returns_normal() {
        let dir = tempfile::tempdir().unwrap();
        let action = check_and_recover(dir.path());
        assert!(matches!(action, RecoveryAction::Normal));
    }

    #[test]
    fn test_check_and_recover_attempts_0_returns_rolling_attempt_1() {
        let dir = tempfile::tempdir().unwrap();
        create_marker(dir.path(), "0.6.2", "0.6.3").unwrap();

        let action = check_and_recover(dir.path());

        match action {
            RecoveryAction::Rolling { from, to, attempt } => {
                assert_eq!(from, "0.6.2");
                assert_eq!(to, "0.6.3");
                assert_eq!(attempt, 1);
            }
            RecoveryAction::Normal => panic!("expected Rolling, got Normal"),
        }

        // Marker file should now have attempts=1
        let path = marker_path(dir.path());
        let contents = std::fs::read_to_string(&path).unwrap();
        let marker: Marker = serde_json::from_str(&contents).unwrap();
        assert_eq!(marker.attempts, 1);
    }

    #[test]
    fn test_check_and_recover_attempts_2_returns_normal_and_deletes_marker() {
        let dir = tempfile::tempdir().unwrap();

        // Write a marker with attempts=2 directly
        let path = marker_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let marker = Marker {
            pending_version: "0.6.3".to_string(),
            previous_version: "0.6.2".to_string(),
            started_at: 1_000_000,
            attempts: 2,
        };
        std::fs::write(&path, serde_json::to_string(&marker).unwrap()).unwrap();

        let action = check_and_recover(dir.path());
        assert!(matches!(action, RecoveryAction::Normal));
        assert!(!path.exists(), "marker should be deleted when attempts >= 2");
    }

    #[test]
    fn test_confirm_ready_deletes_marker() {
        let dir = tempfile::tempdir().unwrap();
        create_marker(dir.path(), "0.6.2", "0.6.3").unwrap();

        let path = marker_path(dir.path());
        assert!(path.exists());

        confirm_ready(dir.path()).unwrap();
        assert!(!path.exists(), "marker should be removed after confirm_ready");
    }

    #[test]
    fn test_confirm_ready_ok_when_no_marker() {
        let dir = tempfile::tempdir().unwrap();
        // No marker file — should succeed without error
        let result = confirm_ready(dir.path());
        assert!(result.is_ok());
    }
}
