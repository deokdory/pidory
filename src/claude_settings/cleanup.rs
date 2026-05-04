//! 시작 시 호출 (P1.5에서 startup 등록). 24h+ 오래된 .tmp leftover만 삭제.
//! 다른 instance가 작업 중일 가능성 있는 신선한 tmp는 보존.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::claude_settings::ClaudeSettingsError;

#[allow(dead_code)]
const STALE_THRESHOLD: Duration = Duration::from_secs(24 * 60 * 60);

#[allow(dead_code)]
const TMP_PREFIXES: &[&str] = &[
    ".settings.json.tmp.",
    ".settings.local.json.tmp.",
];

/// 시작 시 호출 (P1.5에서 startup 등록). 24h+ 오래된 .tmp leftover만 삭제.
/// 다른 instance가 작업 중일 가능성 있는 신선한 tmp는 보존.
///
/// # 동작
/// - `settings_dir` 안의 `.settings.json.tmp.*` 및 `.settings.local.json.tmp.*` 파일 탐색
/// - mtime이 24시간 미만이면 skip (다른 instance 작업 중 가능성)
/// - 24시간 이상이면 unlink (best-effort, 실패 시 warn 로그 후 계속)
/// - 삭제된 파일 개수 반환
#[allow(dead_code)]
pub fn cleanup_leftover_temp(settings_dir: &Path) -> Result<usize, ClaudeSettingsError> {
    let now = SystemTime::now();
    let mut deleted = 0usize;

    for entry in std::fs::read_dir(settings_dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!("cleanup: read_dir entry error: {err}");
                continue;
            }
        };

        let file_name = entry.file_name();
        let name = match file_name.to_str() {
            Some(s) => s,
            None => continue,
        };

        let is_tmp = TMP_PREFIXES.iter().any(|prefix| name.starts_with(prefix));
        if !is_tmp {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(err) => {
                tracing::warn!("cleanup: failed to get metadata for {name}: {err}");
                continue;
            }
        };

        let mtime = match metadata.modified() {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!("cleanup: failed to get mtime for {name}: {err}");
                continue;
            }
        };

        let age = match now.duration_since(mtime) {
            Ok(d) => d,
            Err(_) => {
                // mtime이 미래인 경우 — skip (신선한 것으로 간주)
                continue;
            }
        };

        if age < STALE_THRESHOLD {
            continue;
        }

        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(()) => {
                deleted += 1;
            }
            Err(err) => {
                tracing::warn!("cleanup: failed to remove {}: {err}", path.display());
            }
        }
    }

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::FileTime;
    use std::fs;
    use tempfile::tempdir;

    fn set_mtime_hours_ago(path: &Path, hours: u64) {
        let secs_ago = hours * 3600;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        let past_secs = now.as_secs().saturating_sub(secs_ago);
        let ft = FileTime::from_unix_time(past_secs as i64, 0);
        filetime::set_file_mtime(path, ft).unwrap();
    }

    #[test]
    fn stale_tmp_is_deleted() {
        let dir = tempdir().unwrap();
        let tmp_path = dir.path().join(".settings.json.tmp.123.456");
        fs::write(&tmp_path, b"leftover").unwrap();
        set_mtime_hours_ago(&tmp_path, 25);

        let count = cleanup_leftover_temp(dir.path()).unwrap();
        assert_eq!(count, 1);
        assert!(!tmp_path.exists(), "stale tmp should have been deleted");
    }

    #[test]
    fn fresh_tmp_is_preserved() {
        let dir = tempdir().unwrap();
        let tmp_path = dir.path().join(".settings.json.tmp.999.111");
        fs::write(&tmp_path, b"in-progress").unwrap();
        // mtime은 현재 시각 (기본값) — 24시간 미만

        let count = cleanup_leftover_temp(dir.path()).unwrap();
        assert_eq!(count, 0);
        assert!(tmp_path.exists(), "fresh tmp should be preserved");
    }

    #[test]
    fn real_settings_json_is_untouched() {
        let dir = tempdir().unwrap();
        let real_path = dir.path().join("settings.json");
        let tmp_path = dir.path().join(".settings.json.tmp.x.y");

        fs::write(&real_path, b"{}").unwrap();
        fs::write(&tmp_path, b"leftover").unwrap();
        set_mtime_hours_ago(&tmp_path, 25);

        let count = cleanup_leftover_temp(dir.path()).unwrap();
        assert_eq!(count, 1);
        assert!(real_path.exists(), "settings.json must not be deleted");
        assert!(!tmp_path.exists(), "stale tmp should be deleted");
    }

    #[test]
    fn both_prefixes_are_cleaned() {
        let dir = tempdir().unwrap();
        let tmp1 = dir.path().join(".settings.json.tmp.a.b");
        let tmp2 = dir.path().join(".settings.local.json.tmp.c.d");

        fs::write(&tmp1, b"old1").unwrap();
        fs::write(&tmp2, b"old2").unwrap();
        set_mtime_hours_ago(&tmp1, 25);
        set_mtime_hours_ago(&tmp2, 48);

        let count = cleanup_leftover_temp(dir.path()).unwrap();
        assert_eq!(count, 2);
        assert!(!tmp1.exists());
        assert!(!tmp2.exists());
    }
}
