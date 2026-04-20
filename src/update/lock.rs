use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(not(target_os = "linux"))]
use std::process::Command;
use std::time::Duration;

use super::Error;

/// Max attempts for the acquire loop. Generous because contention races on
/// empty lock files burn iterations via sleep+retry.
const ACQUIRE_MAX_ATTEMPTS: u32 = 16;
/// Base backoff when we observe an empty lock file (concurrent writer mid-init).
const EMPTY_LOCK_BACKOFF: Duration = Duration::from_millis(5);

/// Holds the update lock. Deletes the lock file on drop.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    pid: u32,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Only delete if the file still contains our PID (protect against overwrites).
        if let Ok(contents) = fs::read_to_string(&self.path) {
            if contents.trim().parse::<u32>().ok() == Some(self.pid) {
                let _ = fs::remove_file(&self.path);
            }
        }
        // File missing or different PID → leave it alone.
    }
}

fn lock_path(worktree: &Path) -> PathBuf {
    worktree.join("target").join("release").join(".update.lock")
}

/// Returns true if the process with `pid` is currently alive.
fn is_alive(pid: u32) -> bool {
    // Use /proc/<pid> on Linux (avoids kill integer overflow for large fake PIDs).
    // Fall back to `kill -0` on other platforms (macOS, etc.).
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// Atomically acquire the update lock for `worktree`.
///
/// Uses `O_CREAT | O_EXCL` (via `create_new(true)`) so only one caller can
/// create the lock file. Stale locks (dead PID / unparseable) are removed and
/// the create is retried a bounded number of times to avoid infinite loops
/// under pathological PID reuse.
pub fn acquire(worktree: &Path) -> Result<LockGuard, Error> {
    let path = lock_path(worktree);

    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::BackupFailed(e.to_string()))?;
    }

    let my_pid = std::process::id();

    for attempt in 0..ACQUIRE_MAX_ATTEMPTS {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(my_pid.to_string().as_bytes())
                    .map_err(|e| Error::BackupFailed(e.to_string()))?;
                // Durable write: crash before fsync could leave a zero-length lock
                // that other processes would treat as stale.
                f.sync_all()
                    .map_err(|e| Error::BackupFailed(e.to_string()))?;

                // Read-back verification: between create_new and this point, another
                // thread that observed our empty file could have removed it and
                // created its own. If the path now maps to a different PID (or is
                // gone), we did not actually acquire the lock → retry.
                match fs::read_to_string(&path) {
                    Ok(check) if check.trim().parse::<u32>().ok() == Some(my_pid) => {
                        return Ok(LockGuard { path, pid: my_pid });
                    }
                    _ => continue,
                }
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                match fs::read_to_string(&path) {
                    Ok(contents) => {
                        let trimmed = contents.trim();
                        if trimmed.is_empty() {
                            // Concurrent creator is mid-write. Back off without
                            // removing the file — removing it here is what causes
                            // double-winner races.
                            std::thread::sleep(EMPTY_LOCK_BACKOFF * (attempt + 1));
                            continue;
                        }
                        match trimmed.parse::<u32>() {
                            Ok(pid) if is_alive(pid) => {
                                return Err(Error::LockHeld(pid));
                            }
                            _ => {
                                // Verified dead or persistently garbage → clean up and retry.
                                let _ = fs::remove_file(&path);
                                continue;
                            }
                        }
                    }
                    Err(e2) if e2.kind() == io::ErrorKind::NotFound => {
                        // Raced with a concurrent cleanup — retry create.
                        continue;
                    }
                    Err(e2) => return Err(Error::BackupFailed(e2.to_string())),
                }
            }
            Err(e) => return Err(Error::BackupFailed(e.to_string())),
        }
    }

    // Excessive contention — treat as transient failure to avoid infinite loop.
    Err(Error::BackupFailed(
        "lock acquisition failed after repeated contention".to_string(),
    ))
}

/// Remove the lock file if it belongs to a dead process.
/// No-op if the file does not exist or belongs to a live process.
pub fn cleanup_stale(worktree: &Path) -> Result<(), Error> {
    let path = lock_path(worktree);

    match fs::read_to_string(&path) {
        Ok(contents) => {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                if !is_alive(pid) {
                    fs::remove_file(&path).map_err(|e| Error::BackupFailed(e.to_string()))?;
                }
                // Live process → leave it alone.
            } else {
                // Unparseable → treat as stale.
                fs::remove_file(&path).map_err(|e| Error::BackupFailed(e.to_string()))?;
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Nothing to clean up.
        }
        Err(e) => return Err(Error::BackupFailed(e.to_string())),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::TempDir;

    fn make_worktree() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        // Pre-create target/release/ so acquire() can write the lock file.
        fs::create_dir_all(dir.path().join("target").join("release")).unwrap();
        dir
    }

    #[test]
    fn acquire_creates_lock_file_with_own_pid() {
        let worktree = make_worktree();
        let guard = acquire(worktree.path()).expect("acquire should succeed");

        let path = lock_path(worktree.path());
        assert!(path.exists(), "lock file should exist after acquire");

        let pid_in_file: u32 = fs::read_to_string(&path)
            .unwrap()
            .trim()
            .parse()
            .expect("PID in lock file should be valid u32");
        assert_eq!(pid_in_file, std::process::id());

        drop(guard);
    }

    #[test]
    fn acquire_twice_returns_lock_held() {
        let worktree = make_worktree();
        let _guard = acquire(worktree.path()).expect("first acquire should succeed");

        match acquire(worktree.path()) {
            Err(Error::LockHeld(pid)) => {
                assert_eq!(pid, std::process::id(), "held by our own PID");
            }
            other => panic!("expected LockHeld, got {:?}", other),
        }
    }

    #[test]
    fn drop_removes_lock_file() {
        let worktree = make_worktree();
        let path = lock_path(worktree.path());

        let guard = acquire(worktree.path()).expect("acquire should succeed");
        assert!(path.exists());

        drop(guard);
        assert!(!path.exists(), "lock file should be gone after drop");
    }

    #[test]
    fn acquire_clears_stale_lock_and_succeeds() {
        let worktree = make_worktree();
        let path = lock_path(worktree.path());

        // Write a fake PID that is guaranteed to be dead (max u32, no such process).
        let dead_pid: u32 = 4_294_967_295;
        fs::write(&path, dead_pid.to_string()).unwrap();

        // acquire should detect stale, remove it, and succeed.
        let guard = acquire(worktree.path()).expect("acquire should succeed after stale cleanup");

        let pid_in_file: u32 = fs::read_to_string(&path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(pid_in_file, std::process::id());

        drop(guard);
    }

    #[test]
    fn cleanup_stale_removes_dead_pid() {
        let worktree = make_worktree();
        let path = lock_path(worktree.path());

        let dead_pid: u32 = 4_294_967_295;
        fs::write(&path, dead_pid.to_string()).unwrap();

        cleanup_stale(worktree.path()).expect("cleanup_stale should succeed");
        assert!(!path.exists(), "stale lock should be removed");
    }

    #[test]
    fn cleanup_stale_preserves_live_lock() {
        let worktree = make_worktree();
        let path = lock_path(worktree.path());

        // Write our own PID — we are alive.
        let my_pid = std::process::id();
        fs::write(&path, my_pid.to_string()).unwrap();

        cleanup_stale(worktree.path()).expect("cleanup_stale should succeed");
        assert!(path.exists(), "live lock should be preserved");

        // Clean up manually.
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn cleanup_stale_is_noop_when_no_file() {
        let worktree = make_worktree();
        // No lock file created.
        cleanup_stale(worktree.path()).expect("cleanup_stale on missing file should be OK");
    }

    /// Concurrent `acquire` from multiple threads must yield exactly one winner.
    /// Regression test for TOCTOU race (#240 review).
    #[test]
    fn acquire_is_atomic_under_contention() {
        let worktree = Arc::new(make_worktree());
        let n = 8usize;
        let barrier = Arc::new(Barrier::new(n));
        let mut handles = Vec::with_capacity(n);

        for _ in 0..n {
            let worktree = Arc::clone(&worktree);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                acquire(worktree.path())
            }));
        }

        let mut ok_count = 0;
        let mut held_count = 0;
        let mut guards = Vec::new();
        for h in handles {
            match h.join().unwrap() {
                Ok(g) => {
                    ok_count += 1;
                    guards.push(g);
                }
                Err(Error::LockHeld(_)) => held_count += 1,
                Err(e) => panic!("unexpected error: {:?}", e),
            }
        }

        assert_eq!(ok_count, 1, "exactly one winner expected");
        assert_eq!(held_count, n - 1, "others must observe LockHeld");

        drop(guards);
    }
}
