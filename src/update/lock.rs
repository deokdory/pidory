use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::Error;

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

/// Acquire the update lock for `worktree`.
///
/// Returns `Err(Error::LockHeld(pid))` if another live process holds the lock.
/// Stale locks (dead PID) are removed automatically.
pub fn acquire(worktree: &Path) -> Result<LockGuard, Error> {
    let path = lock_path(worktree);

    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::BackupFailed(e.to_string()))?;
    }

    // Check existing lock file.
    match fs::read_to_string(&path) {
        Ok(contents) => {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                if is_alive(pid) {
                    return Err(Error::LockHeld(pid));
                }
                // Stale — remove and continue.
            }
            // Unparseable content or dead PID → treat as stale, remove.
            let _ = fs::remove_file(&path);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // No lock file — proceed.
        }
        Err(e) => return Err(Error::BackupFailed(e.to_string())),
    }

    let my_pid = std::process::id();
    fs::write(&path, my_pid.to_string()).map_err(|e| Error::BackupFailed(e.to_string()))?;

    Ok(LockGuard { path, pid: my_pid })
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
}
