//! File locking for atomic read-modify-write on Claude settings files.
//!
//! Two-layer locking strategy:
//!
//! - **L1** (`acquire_path_lock`): in-process serialization via a per-path
//!   `tokio::sync::Mutex` stored in a global registry. Ensures that within the
//!   same process only one task at a time operates on a given canonical path.
//!
//! - **L2** (`flock_with_timeout`): inter-process advisory flock (`LOCK_EX`)
//!   via `std::fs::File::try_lock()` (Rust 1.75+ stable API). Coordinates with
//!   other OS processes that also acquire an exclusive flock on the same file.
//!
//! **Deadlock guarantee**: L1=in-process serialization, L2=inter-process
//! advisory. L1을 acquire한 후 L2 시도하므로 같은 process 내 두 task는
//! L1이 직렬화 → flock deadlock 가능성 0.
//!
//! Use `acquire_both` to obtain both guards in the correct order.
// TODO(#286 T5): L1 in-process + L2 inter-process lock layers (implementation complete)

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OwnedMutexGuard};

use super::error::ClaudeSettingsError;

// ---------------------------------------------------------------------------
// L1 — in-process lock registry
// ---------------------------------------------------------------------------

/// Global per-path mutex registry for in-process serialization (L1).
#[allow(dead_code)]
pub(crate) struct LockRegistry {
    inner: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl LockRegistry {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

static LOCK_REGISTRY: OnceLock<LockRegistry> = OnceLock::new();

/// Returns a reference to the global [`LockRegistry`] singleton.
#[allow(dead_code)]
pub(crate) fn lock_registry() -> &'static LockRegistry {
    LOCK_REGISTRY.get_or_init(LockRegistry::new)
}

/// Acquires the L1 in-process mutex for `path`.
///
/// Steps:
/// 1. Lock the registry `HashMap` (tokio mutex).
/// 2. Look up (or insert) `Arc<Mutex<()>>` for this path.
/// 3. Release the registry lock (drop).
/// 4. `.lock_owned().await` on the per-path mutex → returns `OwnedMutexGuard`.
#[allow(dead_code)]
pub(crate) async fn acquire_path_lock(path: &Path) -> OwnedMutexGuard<()> {
    let registry = lock_registry();
    let per_path_arc = {
        let mut map = registry.inner.lock().await;
        map.entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    per_path_arc.lock_owned().await
}

// ---------------------------------------------------------------------------
// L2 — inter-process advisory flock
// ---------------------------------------------------------------------------

/// Acquires an exclusive advisory flock (`LOCK_EX`) on `file`, retrying once
/// after 100 ms if the first attempt times out.
///
/// Each attempt polls `std::fs::File::try_lock()` (non-blocking) until
/// `timeout` elapses.  If both attempts exhaust their timeout, returns
/// [`ClaudeSettingsError::LockConflict`].
///
/// `path` is used only to populate the error variant; the flock is applied to
/// `file`'s file descriptor.
///
/// Uses Rust 1.75+ `std::fs::File::try_lock()` — no external crate needed.
#[allow(dead_code)]
pub(crate) async fn flock_with_timeout(
    file: &File,
    path: &Path,
    timeout: Duration,
) -> Result<(), ClaudeSettingsError> {
    /// Poll `try_lock()` (non-blocking) until `deadline`.  Returns `true` if
    /// the lock was acquired before the deadline.
    async fn try_until(file: &File, deadline: Instant) -> Result<bool, ClaudeSettingsError> {
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(true),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                    // Yield briefly to avoid busy-spinning.
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(ClaudeSettingsError::Io(e));
                }
            }
        }
    }

    // First attempt.
    if try_until(file, Instant::now() + timeout).await? {
        return Ok(());
    }

    // Discard any partial lock state, sleep 100 ms, then retry once.
    let _ = file.unlock();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second (and final) attempt.
    if try_until(file, Instant::now() + timeout).await? {
        return Ok(());
    }

    let _ = file.unlock();
    Err(ClaudeSettingsError::LockConflict {
        path: path.to_path_buf(),
    })
}

// ---------------------------------------------------------------------------
// Combined helper
// ---------------------------------------------------------------------------

/// Guards returned by [`acquire_both`].
///
/// Dropping this value:
/// - releases the L1 in-process mutex (`_l1` drops → `OwnedMutexGuard` drops).
/// - releases the L2 flock (OS releases the advisory lock when the fd is
///   closed; `_file` owns the fd).
#[allow(dead_code)]
pub(crate) struct LockGuards {
    /// L1 in-process guard.  Must be kept alive until the RMW cycle finishes.
    pub(crate) _l1: OwnedMutexGuard<()>,
    /// L2 flock owner.  Drop to close the fd (OS releases advisory lock).
    pub(crate) _file: File,
}

/// Acquires both L1 and L2 in the correct order.
///
/// `canonical` must be the canonicalized path (used as the L1 registry key).
/// `file` is an already-opened `std::fs::File` on which the flock will be
/// acquired.
#[allow(dead_code)]
pub(crate) async fn acquire_both(
    canonical: &Path,
    file: File,
    timeout: Duration,
) -> Result<LockGuards, ClaudeSettingsError> {
    let l1 = acquire_path_lock(canonical).await;
    flock_with_timeout(&file, canonical, timeout).await?;
    Ok(LockGuards { _l1: l1, _file: file })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use tempfile::NamedTempFile;

    /// T1 — L1 in-process serialization: two tasks contend for the same path;
    /// the second must wait until the first releases.
    #[tokio::test]
    async fn l1_inprocess_serialization() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        // Shared log of acquisition times.
        let log: Arc<tokio::sync::Mutex<Vec<(&'static str, Instant)>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));

        let log1 = log.clone();
        let log2 = log.clone();
        let path1 = path.clone();
        let path2 = path.clone();

        let task1 = tokio::spawn(async move {
            let _guard = acquire_path_lock(&path1).await;
            log1.lock().await.push(("t1-acquired", Instant::now()));
            tokio::time::sleep(Duration::from_millis(100)).await;
            log1.lock().await.push(("t1-release", Instant::now()));
            // _guard drops here, releasing L1
        });

        // Give task1 a head-start so it acquires first.
        tokio::time::sleep(Duration::from_millis(10)).await;

        let task2 = tokio::spawn(async move {
            let _guard = acquire_path_lock(&path2).await;
            log2.lock().await.push(("t2-acquired", Instant::now()));
        });

        task1.await.unwrap();
        task2.await.unwrap();

        let entries = log.lock().await;
        let t1_release = entries
            .iter()
            .find(|(k, _)| *k == "t1-release")
            .map(|(_, t)| *t)
            .expect("t1-release missing");
        let t2_acquired = entries
            .iter()
            .find(|(k, _)| *k == "t2-acquired")
            .map(|(_, t)| *t)
            .expect("t2-acquired missing");

        // t2 must have acquired AFTER t1 released.
        assert!(
            t2_acquired >= t1_release,
            "t2 acquired before t1 released — L1 serialization broken"
        );
    }

    /// T2 — L2 inter-process flock timeout: a background thread holds an
    /// exclusive flock; `flock_with_timeout` must return `LockConflict`.
    ///
    /// Implementation: a std thread acquires `LOCK_EX` via `File::lock()`,
    /// then the async task calls `flock_with_timeout`.  On Linux, flock is
    /// per open-file-description, so two `open()` calls on the same inode from
    /// the same process produce independent descriptions that contend normally.
    #[tokio::test]
    async fn l2_flock_timeout_returns_lock_conflict() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let holder_path = path.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

        // Holder thread: acquires LOCK_EX, signals ready, waits for done.
        let holder = std::thread::spawn(move || {
            let f = File::open(&holder_path).unwrap();
            f.lock().unwrap();
            ready_tx.send(()).unwrap();
            done_rx.recv().unwrap();
            f.unlock().unwrap();
        });

        ready_rx.recv().unwrap();

        // Our attempt: separate fd, short per-attempt timeout → must fail.
        let contender = File::open(&path).unwrap();
        let start = Instant::now();
        let result =
            flock_with_timeout(&contender, &path, Duration::from_millis(300)).await;
        let elapsed = start.elapsed();

        done_tx.send(()).unwrap();
        holder.join().unwrap();

        assert!(
            matches!(result, Err(ClaudeSettingsError::LockConflict { .. })),
            "expected LockConflict, got: {result:?}"
        );
        // Must have waited at least one timeout period.
        assert!(
            elapsed >= Duration::from_millis(300),
            "elapsed too short: {elapsed:?}"
        );
        // Must finish within a reasonable bound (2 × 300 ms + 100 ms sleep + margin).
        assert!(
            elapsed < Duration::from_secs(5),
            "elapsed too long: {elapsed:?}"
        );
    }

    /// T3 — L1 different paths: two tasks on distinct paths acquire
    /// simultaneously without blocking each other.
    #[tokio::test]
    async fn l1_different_paths_parallel() {
        let tmp_a = NamedTempFile::new().unwrap();
        let tmp_b = NamedTempFile::new().unwrap();
        let path_a = tmp_a.path().to_path_buf();
        let path_b = tmp_b.path().to_path_buf();

        let (tx_a, rx_a) = tokio::sync::oneshot::channel::<Instant>();
        let (tx_b, rx_b) = tokio::sync::oneshot::channel::<Instant>();

        let task_a = tokio::spawn(async move {
            let _guard = acquire_path_lock(&path_a).await;
            tx_a.send(Instant::now()).unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let task_b = tokio::spawn(async move {
            let _guard = acquire_path_lock(&path_b).await;
            tx_b.send(Instant::now()).unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let acquired_a = rx_a.await.unwrap();
        let acquired_b = rx_b.await.unwrap();

        task_a.await.unwrap();
        task_b.await.unwrap();

        // Both should have acquired nearly simultaneously (within 50 ms of each
        // other), proving they did not serialize.
        let diff = if acquired_a > acquired_b {
            acquired_a - acquired_b
        } else {
            acquired_b - acquired_a
        };
        assert!(
            diff < Duration::from_millis(50),
            "tasks serialized on different paths — diff: {diff:?}"
        );
    }
}
