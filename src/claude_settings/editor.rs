//! Top-level atomic editor entry point (add_permission public API).
//!
//! # RMW pipeline (T8 implemented)
//!
//! ```text
//! apply_mutation flow (13-step):
//!  1. canonical_settings_path      — normalize + expand ~ + reject symlink/dir
//!  2. acquire_path_lock (L1)       — in-process serialization
//!  3. ensure_parent_dir            — mkdir -p 0755
//!  4. open file (O_RDWR|O_CREAT)  — create if absent, mode 0644
//!  5. size guard (1 MiB)
//!  6. flock_with_timeout (L2)      — inter-process advisory lock, 5s + 5s retry
//!  7. fingerprint_old              — mtime + sha256 snapshot
//!  8. read + parse JSON            — backup + notify on corrupt
//!  9. mutator(&mut value)          — Fn, idempotent, returns MergeOutcome
//! 10. L4 re-check fingerprint      — if changed: re-read + re-apply (1 retry)
//! 11. write to temp (.{name}.tmp.{pid}.{nano})
//! 12. fsync temp
//! 13. rename temp → canonical (atomic)
//! 14. fsync parent dir
//! 15. flock release (file drop)
//! 16. L1 release (guard drop)
//! ```
//!
//! ## spawn_blocking decision
//!
//! The blocking I/O section (steps 7-14) runs directly in the async context
//! rather than inside `tokio::task::spawn_blocking`.  Rationale:
//! - File size is capped at 1 MiB — blocking duration is negligible (<1 ms).
//! - Passing `&dyn ConflictNotifier` (a fat pointer) into `spawn_blocking`
//!   requires the reference to be `'static`, which cannot be guaranteed for
//!   arbitrary callers.  Wrapping in `Arc` would change the public API.
//! - L2 `flock_with_timeout` is already async (poll-based, yields every 10 ms)
//!   and must run outside any blocking context.
//! - The Tokio docs note that short blocking operations (<1 ms) are acceptable
//!   directly in async tasks without `spawn_blocking`.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::claude_settings::{
    ClaudeSettingsError, ConflictEvent, ConflictNotifier, MergeOutcome,
};
use crate::claude_settings::dedup::{merge_into_allow, normalize_rule, MergeAction};
use crate::claude_settings::integrity::{changed, fingerprint};
use crate::claude_settings::lock::{acquire_path_lock, flock_with_timeout};
use crate::claude_settings::path::{canonical_settings_path, ensure_parent_dir};

const SIZE_LIMIT: u64 = 1_048_576; // 1 MiB
const FLOCK_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Adds a permission rule to the `permissions.allow` array of the Claude
/// settings file at `path`.
///
/// Behavior:
/// - Creates the file (mode 0644) and parent directory (mode 0755) if absent.
/// - Normalizes the rule (canonical form: `Bash(npm *)`, `WebFetch(domain:...)`).
/// - Dedups against existing rules — exact-match string equality after normalization.
/// - Preserves all other top-level fields (`theme`, `model`, `hooks`, etc.).
/// - Returns [`MergeOutcome::Added`] on first insert, [`MergeOutcome::AlreadyPresent`]
///   on duplicate, [`MergeOutcome::ConflictResolved`] if an external write
///   was detected and re-merged.
///
/// # Errors
///
/// See [`ClaudeSettingsError`] for the full set. Notable cases:
/// - [`SymlinkNotSupported`]/[`IsADirectory`] — `path` invalid type.
/// - [`FileTooLarge`] — settings file exceeds 1 MiB.
/// - [`JsonCorrupted`] — existing settings file is invalid JSON; a backup
///   is saved at `<path>.corrupted-<unix_ts>` and `notifier.notify_conflict`
///   fires before the error is returned.
/// - [`LockConflict`] — could not acquire flock within 5s + 5s retry.
///
/// # Example
///
/// ```no_run
/// # use pidory::claude_settings::{add_permission, LoggingNotifier};
/// # use std::path::Path;
/// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
/// add_permission(
///     Path::new("/tmp/settings.json"),
///     "Bash(npm *)",
///     &LoggingNotifier,
/// ).await?;
/// # Ok(()) }
/// ```
#[allow(dead_code)]
pub async fn add_permission(
    path: &Path,
    rule: &str,
    notifier: &dyn ConflictNotifier,
) -> Result<MergeOutcome, ClaudeSettingsError> {
    let rule_owned = rule.to_string();
    apply_mutation(path, notifier, move |value| {
        let normalized = normalize_rule(&rule_owned);

        // Ensure permissions.allow array exists
        let obj = match value.as_object_mut() {
            Some(o) => o,
            None => return MergeOutcome::Added,
        };
        let perms = obj
            .entry("permissions")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let perms_obj = match perms.as_object_mut() {
            Some(o) => o,
            None => return MergeOutcome::Added,
        };
        let allow_val = perms_obj
            .entry("allow")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        let arr = match allow_val {
            serde_json::Value::Array(a) => a,
            _ => return MergeOutcome::Added,
        };

        match merge_into_allow(arr, normalized) {
            MergeAction::Added => MergeOutcome::Added,
            MergeAction::AlreadyPresent => MergeOutcome::AlreadyPresent,
        }
    })
    .await
}

// ---------------------------------------------------------------------------
// RMW core — pub(crate) only (Plan Must NOT Have — P1.4 흡수 방어)
// ---------------------------------------------------------------------------

/// Atomically read-modify-write a Claude settings JSON file.
///
/// `mutator` receives a mutable reference to the parsed JSON value and returns
/// a [`MergeOutcome`] indicating what happened.  It is called as `Fn` (not
/// `FnOnce`) so that L4 conflict resolution can re-apply the mutation on a
/// freshly read value (1 retry).
///
/// # Errors
///
/// See [`ClaudeSettingsError`] variants.
pub(crate) async fn apply_mutation<F>(
    path: &Path,
    notifier: &dyn ConflictNotifier,
    mutator: F,
) -> Result<MergeOutcome, ClaudeSettingsError>
where
    F: Fn(&mut serde_json::Value) -> MergeOutcome + Send + Sync + 'static,
{
    // ── Step 1: canonical path ──────────────────────────────────────────────
    let canonical = canonical_settings_path(path)?;

    // ── Step 2: L1 in-process lock ──────────────────────────────────────────
    let _l1_guard = acquire_path_lock(&canonical).await;

    // ── Step 3: ensure parent dir ──────────────────────────────────────────
    ensure_parent_dir(&canonical)?;

    // ── Step 4: open file (create if absent, mode 0644) ────────────────────
    // `create(true)` without `truncate` is intentional: we read existing content
    // before writing. The atomic write goes to a separate temp file.
    #[allow(clippy::suspicious_open_options)]
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o644)
        .open(&canonical)?;

    // Newly created empty file: write `{}` so JSON parse succeeds
    if file.metadata()?.len() == 0 {
        file.write_all(b"{}")?;
        file.seek(SeekFrom::Start(0))?;
    }

    // ── Step 5: size guard ─────────────────────────────────────────────────
    let file_size = file.metadata()?.len();
    if file_size > SIZE_LIMIT {
        return Err(ClaudeSettingsError::FileTooLarge {
            path: canonical.clone(),
            size: file_size,
            limit: SIZE_LIMIT,
        });
    }

    // ── Step 6: L2 flock (inter-process advisory) ──────────────────────────
    if let Err(e) = flock_with_timeout(&file, &canonical, FLOCK_TIMEOUT).await {
        notifier.notify_conflict(ConflictEvent::LockTimeout {
            path: canonical.clone(),
            waited: FLOCK_TIMEOUT,
        });
        return Err(e);
    }

    // ── Step 7: fingerprint_old ─────────────────────────────────────────────
    let fingerprint_old = fingerprint(&mut file)?;

    // ── Step 8: read + parse JSON ──────────────────────────────────────────
    file.seek(SeekFrom::Start(0))?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)?;

    let mut value: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(serde_err) => {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = canonical.with_extension(format!("corrupted-{ts}"));
            let _ = std::fs::rename(&canonical, &backup);
            notifier.notify_conflict(ConflictEvent::JsonCorrupted {
                path: canonical.clone(),
                backup: backup.clone(),
                reason: serde_err.to_string(),
            });
            return Err(ClaudeSettingsError::JsonCorrupted {
                path: canonical,
                source: serde_err,
                backup,
            });
        }
    };

    // ── Step 9: apply mutator ──────────────────────────────────────────────
    let mut outcome = mutator(&mut value);

    // ── Step 10: L4 re-check fingerprint ──────────────────────────────────
    let fingerprint_new = fingerprint(&mut file)?;
    if changed(&fingerprint_old, &fingerprint_new) {
        // External modification detected — re-read and retry once
        file.seek(SeekFrom::Start(0))?;
        let mut raw2 = Vec::new();
        file.read_to_end(&mut raw2)?;

        match serde_json::from_slice::<serde_json::Value>(&raw2) {
            Ok(mut fresh_value) => {
                mutator(&mut fresh_value);
                value = fresh_value;
                outcome = MergeOutcome::ConflictResolved;
            }
            Err(_) => {
                notifier.notify_conflict(ConflictEvent::MtimeChanged {
                    path: canonical.clone(),
                    attempted_rule: "<unknown>".to_string(),
                });
                return Err(ClaudeSettingsError::MtimeChangedDuringRmw { path: canonical });
            }
        }
    }

    // ── Steps 11-14: write temp → fsync → rename → parent fsync ───────────
    write_atomic(&canonical, &value, notifier)?;

    // ── Step 15: flock release ─────────────────────────────────────────────
    let _ = file.unlock();
    // ── Step 16: L1 guard drops when _l1_guard goes out of scope ────────────

    Ok(outcome)
}

/// Writes `value` atomically to `canonical` via a temp file.
///
/// Steps 11-14:
/// - Create `.{name}.tmp.{pid}.{nano}` in same directory
/// - Write serialized JSON
/// - fsync temp file
/// - rename temp → canonical (atomic)
/// - fsync parent directory
fn write_atomic(
    canonical: &Path,
    value: &serde_json::Value,
    notifier: &dyn ConflictNotifier,
) -> Result<(), ClaudeSettingsError> {
    let canonical_parent = canonical.parent().ok_or_else(|| ClaudeSettingsError::InvalidPath {
        path: canonical.to_path_buf(),
        reason: "no parent dir".to_string(),
    })?;

    let target_filename = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("settings.json");

    let nano = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let temp_name = format!(".{}.tmp.{}.{}", target_filename, pid, nano);
    let temp_path = canonical_parent.join(&temp_name);

    // ── Step 11: write to temp ─────────────────────────────────────────────
    let mut temp_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temp_path)?;

    let serialized = serde_json::to_vec_pretty(value)
        .map_err(|e| ClaudeSettingsError::Io(std::io::Error::other(e.to_string())))?;
    temp_file.write_all(&serialized)?;

    // ── Step 12: fsync temp ────────────────────────────────────────────────
    if let Err(e) = temp_file.sync_all() {
        notifier.notify_conflict(ConflictEvent::FsyncFailed {
            path: canonical.to_path_buf(),
            source: e.to_string(),
        });
        let _ = std::fs::remove_file(&temp_path);
        return Err(ClaudeSettingsError::Io(e));
    }

    // Close temp fd before rename
    drop(temp_file);

    // ── Step 13: atomic rename ─────────────────────────────────────────────
    if let Err(e) = std::fs::rename(&temp_path, canonical) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(ClaudeSettingsError::Io(e));
    }

    // ── Step 14: fsync parent dir ──────────────────────────────────────────
    let parent_file = OpenOptions::new().read(true).open(canonical_parent)?;
    if let Err(e) = parent_file.sync_all() {
        notifier.notify_conflict(ConflictEvent::FsyncFailed {
            path: canonical_parent.to_path_buf(),
            source: e.to_string(),
        });
        return Err(ClaudeSettingsError::Io(e));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod race_tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs::File;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    use crate::claude_settings::LoggingNotifier;

    /// TestNotifier captures events for assertions.
    struct TestNotifier {
        events: Mutex<Vec<String>>,
    }

    impl TestNotifier {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    impl ConflictNotifier for TestNotifier {
        fn notify_conflict(&self, event: ConflictEvent) {
            let label = match &event {
                ConflictEvent::MtimeChanged { .. } => "MtimeChanged",
                ConflictEvent::LockTimeout { .. } => "LockTimeout",
                ConflictEvent::JsonCorrupted { .. } => "JsonCorrupted",
                ConflictEvent::FsyncFailed { .. } => "FsyncFailed",
            };
            self.events.lock().unwrap().push(label.to_string());
        }
    }

    // ── AC1: in-process 100 task race ────────────────────────────────────────
    /// 100개 tokio task가 동시에 서로 다른 rule을 add → 최종 allow 길이 == 100, 중복 0.
    /// 10 라운드 반복 (flaky 감지).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ac1_in_process_100_task_race() {
        for _round in 0..10 {
            let tmp = TempDir::new().unwrap();
            let path = tmp.path().join("settings.json");
            std::fs::write(&path, b"{}").unwrap();

            let notifier = Arc::new(LoggingNotifier);
            let mut handles = Vec::new();
            for i in 0..100u32 {
                let path = path.clone();
                let notifier = notifier.clone();
                handles.push(tokio::spawn(async move {
                    let rule = format!("Bash(rule_{})", i);
                    add_permission(&path, &rule, &*notifier).await
                }));
            }
            for h in handles {
                h.await.unwrap().expect("add_permission failed");
            }

            let content = std::fs::read_to_string(&path).unwrap();
            let v: serde_json::Value = serde_json::from_str(&content).unwrap();
            let allow = v["permissions"]["allow"].as_array().unwrap();
            assert_eq!(
                allow.len(),
                100,
                "round {_round}: expected 100 unique rules, got {}",
                allow.len()
            );
            let mut set = HashSet::new();
            for r in allow {
                let s = r.as_str().unwrap().to_string();
                assert!(set.insert(s.clone()), "duplicate in round {_round}: {s}");
            }
        }
    }

    // ── AC2: inter-process race (separate tokio runtimes in threads) ──────────
    /// 2개 std::thread (각자 별도 tokio runtime)가 각각 50개 rule add → 최종 allow == 100.
    ///
    /// Note: 실제 OS child process spawn은 별도 binary 빌드 필요 + 복잡도가 높아
    /// 별도 tokio runtime을 갖는 std::thread로 시뮬레이션.
    /// L1 lock registry는 per-process 전역이라 별도 thread의 별도 runtime은
    /// L1 공유가 안 됨 → L2 flock만으로 직렬화. 이는 실제 inter-process 동작과 동일.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ac2_inter_process_race_simulated() {
        for _round in 0..10 {
            let tmp = TempDir::new().unwrap();
            let path = tmp.path().join("settings.json");
            std::fs::write(&path, b"{}").unwrap();

            let path_a = path.clone();
            let path_b = path.clone();

            let handle_a = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let mut hs = Vec::new();
                    for i in 0..50u32 {
                        let p = path_a.clone();
                        let rule = format!("Bash(proc_a_rule_{})", i);
                        hs.push(tokio::spawn(async move {
                            add_permission(&p, &rule, &LoggingNotifier).await
                        }));
                    }
                    for h in hs {
                        h.await.unwrap().expect("add_permission a failed");
                    }
                });
            });

            let handle_b = std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let mut hs = Vec::new();
                    for i in 0..50u32 {
                        let p = path_b.clone();
                        let rule = format!("Bash(proc_b_rule_{})", i);
                        hs.push(tokio::spawn(async move {
                            add_permission(&p, &rule, &LoggingNotifier).await
                        }));
                    }
                    for h in hs {
                        h.await.unwrap().expect("add_permission b failed");
                    }
                });
            });

            handle_a.join().unwrap();
            handle_b.join().unwrap();

            let content = std::fs::read_to_string(&path).unwrap();
            let v: serde_json::Value = serde_json::from_str(&content).unwrap();
            let allow = v["permissions"]["allow"].as_array().unwrap();
            assert_eq!(
                allow.len(),
                100,
                "round {_round}: expected 100 rules after 2-thread race, got {}",
                allow.len()
            );
            let mut set = HashSet::new();
            for r in allow {
                let s = r.as_str().unwrap().to_string();
                assert!(set.insert(s.clone()), "duplicate in round {_round}: {s}");
            }
        }
    }

    // ── AC3: SIGKILL simulation (leftover .tmp file) ───────────────────────────
    /// SIGKILL-during-write를 직접 구현하는 대신, 이전 write가 중단돼 남은
    /// .tmp leftover가 존재하는 상황을 시뮬레이션.
    ///
    /// Note: 실제 child process SIGKILL은 별도 binary 필요. 여기서는
    /// stale .tmp 파일이 존재해도 settings.json이 손상 없이 add_permission 성공,
    /// JSON valid임을 확인. leftover는 cleanup_leftover_temp(P1.5)가 처리.
    #[tokio::test]
    async fn ac3_sigkill_simulation_tmp_leftover() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, b"{}").unwrap();

        // Simulate leftover .tmp from a killed writer (stale, >24h)
        let leftover = tmp.path().join(".settings.json.tmp.99999.123456789");
        std::fs::write(&leftover, b"PARTIAL WRITE GARBAGE {{{").unwrap();
        let old_time = filetime::FileTime::from_unix_time(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .saturating_sub(90_000) as i64,
            0,
        );
        filetime::set_file_mtime(&leftover, old_time).unwrap();

        let notifier = LoggingNotifier;
        add_permission(&path, "Bash(ls)", &notifier)
            .await
            .expect("add_permission should succeed despite leftover .tmp");

        let content = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&content).expect("settings.json must be valid JSON");
        let allow = v["permissions"]["allow"].as_array().unwrap();
        assert_eq!(allow.len(), 1);
        assert_eq!(allow[0], serde_json::Value::String("Bash(ls)".to_string()));

        // leftover still exists (cleanup is separate)
        assert!(leftover.exists(), "leftover .tmp should still exist — cleanup handles it separately");
    }

    // ── AC4: vim contention — LOCK_EX held by another fd ──────────────────────
    /// 별도 thread가 LOCK_EX를 획득한 채 대기 → add_permission 호출 →
    /// flock_with_timeout (5s + 5s) 내에 LockConflict + TestNotifier에 LockTimeout 1개.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ac4_vim_contention_lock_conflict() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, b"{}").unwrap();

        let holder_path = path.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

        // Holder thread: acquire LOCK_EX on a separate fd, hold until signalled
        let holder = std::thread::spawn(move || {
            let f = File::open(&holder_path).unwrap();
            f.lock().unwrap();
            ready_tx.send(()).unwrap();
            // Hold lock long enough for both flock_with_timeout attempts (5s + 5s) to expire
            done_rx.recv().unwrap();
            f.unlock().unwrap();
        });

        ready_rx.recv().unwrap();

        let notifier = Arc::new(TestNotifier::new());
        let notifier_ref: &dyn ConflictNotifier = &*notifier;

        let result = add_permission(&path, "Bash(vim_test)", notifier_ref).await;

        done_tx.send(()).unwrap();
        holder.join().unwrap();

        assert!(
            matches!(result, Err(ClaudeSettingsError::LockConflict { .. })),
            "expected LockConflict, got: {result:?}"
        );

        let events = notifier.events();
        assert_eq!(events.len(), 1, "expected exactly 1 LockTimeout event, got: {events:?}");
        assert_eq!(events[0], "LockTimeout");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use tempfile::TempDir;

    use crate::claude_settings::{LoggingNotifier, MergeOutcome};

    // ── IT1: 빈 파일에 첫 add ────────────────────────────────────────────────
    /// `{}` 파일에 `Bash(npm *)` add → `MergeOutcome::Added`,
    /// 파일 내용 = `{"permissions":{"allow":["Bash(npm *)"]}}` 검증.
    #[tokio::test]
    async fn it1_empty_file_first_add() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, b"{}").unwrap();

        let outcome = add_permission(&path, "Bash(npm *)", &LoggingNotifier)
            .await
            .expect("add_permission should succeed");

        assert_eq!(outcome, MergeOutcome::Added);

        let content = std::fs::read_to_string(&path).unwrap();
        let actual: serde_json::Value = serde_json::from_str(&content).unwrap();
        let expected: serde_json::Value = serde_json::json!({
            "permissions": {
                "allow": ["Bash(npm *)"]
            }
        });
        assert_eq!(actual, expected, "file content mismatch after first add");
    }

    // ── IT2: 중복 add (정규화 후 동일 rule) ──────────────────────────────────
    /// IT1 후 `Bash(npm:*)` add (정규화 시 `Bash(npm *)` 동일) →
    /// `MergeOutcome::AlreadyPresent`, allow.len() == 1 유지.
    #[tokio::test]
    async fn it2_duplicate_add_already_present() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, b"{}").unwrap();

        // 첫 add
        let first = add_permission(&path, "Bash(npm *)", &LoggingNotifier)
            .await
            .expect("first add_permission should succeed");
        assert_eq!(first, MergeOutcome::Added);

        // 중복 add — 정규화 후 동일한 rule (Bash(npm:*) → Bash(npm *))
        let second = add_permission(&path, "Bash(npm:*)", &LoggingNotifier)
            .await
            .expect("second add_permission should succeed");
        assert_eq!(second, MergeOutcome::AlreadyPresent);

        // 파일 변화 없음 — allow.len() == 1
        let content = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = v["permissions"]["allow"].as_array().unwrap();
        assert_eq!(allow.len(), 1, "allow array should have exactly 1 entry after duplicate add");
    }

    // ── IT3: 다른 필드 보존 ───────────────────────────────────────────────────
    /// `{"theme":"dark","model":"claude-sonnet"}` → `Bash(ls)` add →
    /// 파일 = `{"theme":"dark","model":"claude-sonnet","permissions":{"allow":["Bash(ls)"]}}` 검증.
    #[tokio::test]
    async fn it3_other_fields_preserved() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, br#"{"theme":"dark","model":"claude-sonnet"}"#).unwrap();

        let outcome = add_permission(&path, "Bash(ls)", &LoggingNotifier)
            .await
            .expect("add_permission should succeed");
        assert_eq!(outcome, MergeOutcome::Added);

        let content = std::fs::read_to_string(&path).unwrap();
        let actual: serde_json::Value = serde_json::from_str(&content).unwrap();
        let expected: serde_json::Value = serde_json::json!({
            "theme": "dark",
            "model": "claude-sonnet",
            "permissions": {
                "allow": ["Bash(ls)"]
            }
        });
        assert_eq!(actual, expected, "other fields should be preserved after add");
    }

    // ── IT4: permissions 필드만 부재 ─────────────────────────────────────────
    /// `{"theme":"dark"}` → `Bash(ls)` add →
    /// `{"theme":"dark","permissions":{"allow":["Bash(ls)"]}}` 검증.
    #[tokio::test]
    async fn it4_permissions_field_absent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, br#"{"theme":"dark"}"#).unwrap();

        let outcome = add_permission(&path, "Bash(ls)", &LoggingNotifier)
            .await
            .expect("add_permission should succeed");
        assert_eq!(outcome, MergeOutcome::Added);

        let content = std::fs::read_to_string(&path).unwrap();
        let actual: serde_json::Value = serde_json::from_str(&content).unwrap();
        let expected: serde_json::Value = serde_json::json!({
            "theme": "dark",
            "permissions": {
                "allow": ["Bash(ls)"]
            }
        });
        assert_eq!(actual, expected, "theme field should be preserved when adding to permissions-absent file");
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    use crate::claude_settings::{ClaudeSettingsError, LoggingNotifier, MergeOutcome};

    /// TestNotifier captures events for assertions.
    struct TestNotifier {
        events: Mutex<Vec<String>>,
    }

    impl TestNotifier {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    impl ConflictNotifier for TestNotifier {
        fn notify_conflict(&self, event: ConflictEvent) {
            let label = match &event {
                ConflictEvent::MtimeChanged { .. } => "MtimeChanged",
                ConflictEvent::LockTimeout { .. } => "LockTimeout",
                ConflictEvent::JsonCorrupted { .. } => "JsonCorrupted",
                ConflictEvent::FsyncFailed { .. } => "FsyncFailed",
            };
            self.events.lock().unwrap().push(label.to_string());
        }
    }

    // ── EC1: EACCES (read-only parent dir) ──────────────────────────────────
    /// parent dir을 chmod 0o555로 만들어 write 불가 → add_permission이 Err 반환.
    /// cleanup: chmod 0o755로 복구 (TempDir Drop 안전).
    #[tokio::test]
    async fn ec1_eacces_readonly_parent_dir() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        let path = dir.join("settings.json");

        // Make parent dir read-only (no write)
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = add_permission(&path, "Bash(ls)", &LoggingNotifier).await;

        // Cleanup: restore write permission before TempDir drops (to avoid panic)
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            result.is_err(),
            "expected Err for read-only parent dir, got Ok"
        );
    }

    // ── EC2: EISDIR (path가 dir) ─────────────────────────────────────────────
    /// tempdir/settings.json을 dir로 생성 → add_permission → IsADirectory error.
    #[tokio::test]
    async fn ec2_eisdir_path_is_directory() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        // Create settings.json as a directory, not a file
        std::fs::create_dir(&path).unwrap();

        let result = add_permission(&path, "Bash(ls)", &LoggingNotifier).await;

        assert!(
            matches!(result, Err(ClaudeSettingsError::IsADirectory { .. })),
            "expected IsADirectory, got: {result:?}"
        );
    }

    // ── EC3: Symlink 거부 ────────────────────────────────────────────────────
    /// tempdir/settings.json → /tmp/real symlink → add_permission → SymlinkNotSupported error.
    #[tokio::test]
    async fn ec3_symlink_rejected() {
        let tmp = TempDir::new().unwrap();
        let real_target = tmp.path().join("real_target.json");
        std::fs::write(&real_target, b"{}").unwrap();

        let link_path = tmp.path().join("settings.json");
        std::os::unix::fs::symlink(&real_target, &link_path).unwrap();

        let result = add_permission(&link_path, "Bash(ls)", &LoggingNotifier).await;

        assert!(
            matches!(result, Err(ClaudeSettingsError::SymlinkNotSupported { .. })),
            "expected SymlinkNotSupported, got: {result:?}"
        );
    }

    // ── EC4: Parent dir 부재 → mkdir_p ──────────────────────────────────────
    /// tempdir/sub1/sub2/settings.json (sub1 없음) → add_permission → 성공, dir 자동 생성.
    #[tokio::test]
    async fn ec4_missing_parent_dirs_created() {
        let tmp = TempDir::new().unwrap();
        let deep_path = tmp.path().join("sub1").join("sub2").join("settings.json");

        // Precondition: sub1 does not exist
        assert!(
            !tmp.path().join("sub1").exists(),
            "precondition: sub1 should not exist"
        );

        let result = add_permission(&deep_path, "Bash(ls)", &LoggingNotifier).await;

        assert_eq!(
            result.unwrap(),
            MergeOutcome::Added,
            "expected MergeOutcome::Added after auto-creating parent dirs"
        );

        assert!(
            deep_path.parent().unwrap().exists(),
            "sub1/sub2 directory should have been created"
        );
    }

    // ── EC5: 파일 부재 → 자동 생성 ──────────────────────────────────────────
    /// tempdir/settings.json 없음 → add_permission → 성공, 파일 생성됨 + JSON 구조 검증.
    #[tokio::test]
    async fn ec5_file_absent_auto_created() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        // Precondition: file does not exist
        assert!(!path.exists(), "precondition: settings.json should not exist");

        let result = add_permission(&path, "Bash(ls)", &LoggingNotifier).await;

        assert_eq!(
            result.unwrap(),
            MergeOutcome::Added,
            "expected MergeOutcome::Added for absent file"
        );

        assert!(path.exists(), "settings.json should have been created");

        let content = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = v["permissions"]["allow"].as_array().unwrap();
        assert_eq!(allow.len(), 1);
        assert_eq!(
            allow[0],
            serde_json::Value::String("Bash(ls)".to_string()),
            "newly created file should contain the added rule"
        );
    }

    // ── EC6: JSON 손상 → backup + error ─────────────────────────────────────
    /// tempdir/settings.json = `{invalid` → add_permission → JsonCorrupted error,
    /// `.corrupted-{ts}` 존재, notifier capture에 1개.
    #[tokio::test]
    async fn ec6_json_corrupted_backup_and_notify() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        // Write invalid JSON
        std::fs::write(&path, b"{invalid json").unwrap();

        let notifier = TestNotifier::new();
        let result = add_permission(&path, "Bash(ls)", &notifier).await;

        match result {
            Err(ClaudeSettingsError::JsonCorrupted { backup, .. }) => {
                assert!(
                    backup.exists(),
                    "backup file should exist at {backup:?}"
                );
            }
            other => panic!("expected JsonCorrupted, got: {other:?}"),
        }

        let events = notifier.events();
        assert_eq!(
            events.len(),
            1,
            "expected exactly 1 notifier event, got: {events:?}"
        );
        assert_eq!(events[0], "JsonCorrupted");
    }

    // ── EC7: 1MB 초과 → FileTooLarge ────────────────────────────────────────
    /// tempdir/settings.json = 1.5 MB의 valid JSON → add_permission → FileTooLarge error.
    #[tokio::test]
    async fn ec7_file_too_large() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        // Build a valid JSON that exceeds 1 MiB (1_048_576 bytes)
        // Use a large array of short strings to reach ~1.5 MB
        // Each entry `"aaaa...",` ≈ 70 bytes. 25_000 entries ≈ 1.75 MB → safely over limit.
        let entry = format!("\"{}\"", "a".repeat(60));
        let entries_count = 25_000usize;
        let mut json = String::with_capacity(entries_count * 64 + 20);
        json.push_str("{\"data\":[");
        for i in 0..entries_count {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&entry);
        }
        json.push_str("]}");

        // Ensure we actually exceed the limit
        assert!(
            json.len() > 1_048_576,
            "test JSON should exceed 1 MiB, got {} bytes",
            json.len()
        );

        std::fs::write(&path, json.as_bytes()).unwrap();

        let result = add_permission(&path, "Bash(ls)", &LoggingNotifier).await;

        match result {
            Err(ClaudeSettingsError::FileTooLarge { size, limit, .. }) => {
                assert!(size > limit, "size ({size}) should exceed limit ({limit})");
            }
            other => panic!("expected FileTooLarge, got: {other:?}"),
        }
    }

    // ── EC8: process wrapper raw forward ────────────────────────────────────
    /// `add_permission(path, "Bash(timeout 30 npm test)", ...)` →
    /// `Bash(timeout 30 npm test)` 그대로 저장 (split/strip 시도 X, P1.6 명세 검증).
    #[tokio::test]
    async fn ec8_process_wrapper_raw_forward() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, b"{}").unwrap();

        let result =
            add_permission(&path, "Bash(timeout 30 npm test)", &LoggingNotifier).await;

        assert_eq!(
            result.unwrap(),
            MergeOutcome::Added,
            "expected MergeOutcome::Added for process wrapper rule"
        );

        let content = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = v["permissions"]["allow"].as_array().unwrap();
        assert_eq!(allow.len(), 1);
        assert_eq!(
            allow[0],
            serde_json::Value::String("Bash(timeout 30 npm test)".to_string()),
            "process wrapper rule should be stored verbatim without stripping"
        );
    }
}
