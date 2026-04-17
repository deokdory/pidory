use std::path::{Path, PathBuf};
use std::process::Command;

use super::Error;

pub fn backup_binary(worktree: &Path) -> Result<PathBuf, Error> {
    let src = worktree.join("target/release/pidory");
    let dst = worktree.join("target/release/pidory.backup");
    let tmp = worktree.join("target/release/pidory.backup.tmp");

    std::fs::copy(&src, &tmp).map_err(|e| {
        Error::BackupFailed(format!("copy {} → {} failed: {}", src.display(), tmp.display(), e))
    })?;

    std::fs::rename(&tmp, &dst).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::BackupFailed(format!(
            "rename {} → {} failed: {}",
            tmp.display(),
            dst.display(),
            e
        ))
    })?;

    Ok(dst)
}

pub fn backup_db(db_path: &Path) -> Result<PathBuf, Error> {
    let backup_path = {
        let mut p = db_path.as_os_str().to_owned();
        p.push(".backup");
        PathBuf::from(p)
    };

    let db_str = db_path.to_string_lossy();
    let backup_str = backup_path.to_string_lossy();
    let dot_cmd = format!(".backup '{}'", backup_str);

    let output = Command::new("sqlite3")
        .args([db_str.as_ref(), dot_cmd.as_str()])
        .output()
        .map_err(|e| Error::BackupFailed(format!("sqlite3 spawn failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(Error::BackupFailed(format!("sqlite3 .backup failed: {}", stderr)));
    }

    Ok(backup_path)
}

pub fn restore_binary(worktree: &Path) -> Result<(), Error> {
    let src = worktree.join("target/release/pidory.backup");
    let dst = worktree.join("target/release/pidory");

    if !src.exists() {
        return Err(Error::BackupFailed("no backup to restore".to_string()));
    }

    std::fs::rename(&src, &dst).map_err(|e| {
        Error::BackupFailed(format!(
            "rename {} → {} failed: {}",
            src.display(),
            dst.display(),
            e
        ))
    })?;

    Ok(())
}

pub fn restore_db(db_path: &Path) -> Result<(), Error> {
    let backup_path = {
        let mut p = db_path.as_os_str().to_owned();
        p.push(".backup");
        PathBuf::from(p)
    };

    if !backup_path.exists() {
        return Err(Error::BackupFailed("no db backup to restore".to_string()));
    }

    std::fs::copy(&backup_path, db_path).map_err(|e| {
        Error::BackupFailed(format!(
            "copy {} → {} failed: {}",
            backup_path.display(),
            db_path.display(),
            e
        ))
    })?;

    // Remove stale WAL/SHM files so they don't overwrite the restored DB.
    for ext in &["-wal", "-shm"] {
        let mut p = db_path.as_os_str().to_owned();
        p.push(ext);
        let wal = PathBuf::from(p);
        if wal.exists() {
            let _ = std::fs::remove_file(&wal);
        }
    }

    Ok(())
}

pub fn check_disk_space(worktree: &Path, min_bytes: u64) -> Result<(), Error> {
    #[cfg(target_os = "linux")]
    {
        let output = Command::new("df")
            .args(["--output=avail", "-B1", &worktree.to_string_lossy()])
            .output()
            .map_err(|e| Error::BackupFailed(format!("df spawn failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(Error::BackupFailed(format!("df failed: {}", stderr)));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Output is two lines: header "Avail" + value
        let avail: u64 = stdout
            .lines()
            .nth(1)
            .and_then(|l| l.trim().parse().ok())
            .ok_or_else(|| Error::BackupFailed(format!("df output unparseable: {}", stdout)))?;

        if avail < min_bytes {
            return Err(Error::InsufficientDiskSpace);
        }

        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (worktree, min_bytes);
        unimplemented!("check_disk_space is only supported on Linux")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn sqlite3_available() -> bool {
        Command::new("sqlite3").arg("--version").output().is_ok()
    }

    fn make_fake_binary(dir: &Path) -> PathBuf {
        let release_dir = dir.join("target/release");
        fs::create_dir_all(&release_dir).unwrap();
        let bin = release_dir.join("pidory");
        let mut f = fs::File::create(&bin).unwrap();
        f.write_all(b"fake-binary-content").unwrap();
        bin
    }

    #[test]
    fn test_backup_binary_creates_backup_with_same_content() {
        let dir = tempfile::tempdir().unwrap();
        make_fake_binary(dir.path());

        let dst = backup_binary(dir.path()).unwrap();

        assert!(dst.exists());
        let content = fs::read(&dst).unwrap();
        assert_eq!(content, b"fake-binary-content");
    }

    #[test]
    fn test_restore_binary_restores_from_backup() {
        let dir = tempfile::tempdir().unwrap();
        make_fake_binary(dir.path());

        // backup → backup file contains original content
        backup_binary(dir.path()).unwrap();

        // overwrite original with new content
        let bin = dir.path().join("target/release/pidory");
        fs::write(&bin, b"new-content").unwrap();

        // restore → original should hold backup content again
        restore_binary(dir.path()).unwrap();

        let restored = fs::read(&bin).unwrap();
        assert_eq!(restored, b"fake-binary-content");

        // backup file is consumed by rename
        let backup = dir.path().join("target/release/pidory.backup");
        assert!(!backup.exists());
    }

    #[test]
    fn test_restore_binary_missing_backup_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("target/release")).unwrap();

        let result = restore_binary(dir.path());
        assert!(matches!(result, Err(Error::BackupFailed(_))));
    }

    #[test]
    fn test_backup_db_and_restore_db() {
        if !sqlite3_available() {
            eprintln!("sqlite3 not found — skipping test_backup_db_and_restore_db");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("pidory.db");

        // create a minimal sqlite DB
        let status = Command::new("sqlite3")
            .args([db_path.to_str().unwrap(), "CREATE TABLE t(x);"])
            .status()
            .unwrap();
        assert!(status.success());

        let backup_path = backup_db(&db_path).unwrap();
        assert!(backup_path.exists());

        // verify backup is a valid SQLite file (sqlite3 .tables exits 0)
        let verify = Command::new("sqlite3")
            .args([backup_path.to_str().unwrap(), ".tables"])
            .output()
            .unwrap();
        assert!(verify.status.success());

        // restore: overwrite db, then restore
        fs::write(&db_path, b"corrupted").unwrap();
        restore_db(&db_path).unwrap();

        // restored db should be valid again
        let check = Command::new("sqlite3")
            .args([db_path.to_str().unwrap(), ".tables"])
            .output()
            .unwrap();
        assert!(check.status.success());
    }

    #[test]
    fn test_restore_db_removes_stale_wal() {
        if !sqlite3_available() {
            eprintln!("sqlite3 not found — skipping test_restore_db_removes_stale_wal");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("pidory.db");

        Command::new("sqlite3")
            .args([db_path.to_str().unwrap(), "CREATE TABLE t(x);"])
            .status()
            .unwrap();

        backup_db(&db_path).unwrap();

        // plant fake WAL/SHM files
        fs::write(dir.path().join("pidory.db-wal"), b"stale").unwrap();
        fs::write(dir.path().join("pidory.db-shm"), b"stale").unwrap();

        restore_db(&db_path).unwrap();

        assert!(!dir.path().join("pidory.db-wal").exists());
        assert!(!dir.path().join("pidory.db-shm").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_check_disk_space_zero_min_ok() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_disk_space(dir.path(), 0).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_check_disk_space_max_min_err() {
        let dir = tempfile::tempdir().unwrap();
        let result = check_disk_space(dir.path(), u64::MAX);
        assert!(matches!(result, Err(Error::InsufficientDiskSpace)));
    }
}
