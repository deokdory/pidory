use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::Error;
use super::db_url::parse_pg_url;

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

/// Postgres DB를 backup_dir/pidory-backup.sql 로 pg_dump한다.
/// 호출자가 backup_dir 결정. 파일명 고정.
pub fn backup_db(database_url: &str, backup_dir: &Path) -> Result<PathBuf, Error> {
    let parts = parse_pg_url(database_url)?;
    let backup_path = backup_dir.join("pidory-backup.sql");

    let port_str = parts.port.to_string();
    let backup_str = backup_path.to_string_lossy().into_owned();

    let mut cmd = Command::new("pg_dump");
    cmd.args([
        "--clean", "--if-exists",
        "-U", &parts.user,
        "-h", &parts.host,
        "-p", &port_str,
        "-d", &parts.dbname,
        "-f", &backup_str,
    ]);
    if let Some(pw) = &parts.password {
        cmd.env("PGPASSWORD", pw);
    }
    let output = cmd
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| Error::BackupFailed(format!("pg_dump spawn 실패: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // stderr는 pg_dump가 출력한 것 — DATABASE_URL/password 원문 미포함 (안전).
        return Err(Error::BackupFailed(format!("pg_dump 실패: {}", stderr.trim())));
    }

    verify_backup_magic(&backup_path)?;
    Ok(backup_path)
}

/// Backup 파일이 유효한 pg_dump 산출물인지 첫 줄 매직으로 검증.
pub(crate) fn verify_backup_magic(path: &Path) -> Result<(), Error> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path)
        .map_err(|e| Error::BackupFailed(format!("백업 파일 열기 실패: {}", e)))?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    reader
        .read_line(&mut first)
        .map_err(|e| Error::BackupFailed(format!("백업 파일 읽기 실패: {}", e)))?;
    // pg_dump --version 11+ 기본 첫 줄: "-- PostgreSQL database dump"
    if !first.starts_with("-- PostgreSQL database dump") {
        return Err(Error::BackupFailed(
            "백업 파일 검증 실패: 매직 라인 불일치".into(),
        ));
    }
    Ok(())
}

/// Postgres backup_path (pidory-backup.sql)을 psql로 restore한다.
pub fn restore_db(database_url: &str, backup_path: &Path) -> Result<(), Error> {
    if !backup_path.exists() {
        return Err(Error::BackupFailed("복원할 백업 파일 없음".into()));
    }
    verify_backup_magic(backup_path)?;
    let parts = parse_pg_url(database_url)?;
    let port_str = parts.port.to_string();
    let backup_str = backup_path.to_string_lossy().into_owned();

    let mut cmd = Command::new("psql");
    cmd.args([
        "-U", &parts.user,
        "-h", &parts.host,
        "-p", &port_str,
        "-d", &parts.dbname,
        "-v", "ON_ERROR_STOP=1",
        "-f", &backup_str,
    ]);
    if let Some(pw) = &parts.password {
        cmd.env("PGPASSWORD", pw);
    }
    let output = cmd
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| Error::BackupFailed(format!("psql spawn 실패: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(Error::BackupFailed(format!("psql restore 실패: {}", stderr.trim())));
    }

    Ok(())
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
        // macOS/BSD에서는 `df --output=avail` 플래그가 없어 best-effort로 통과.
        // 실제 디스크 부족은 뒤따르는 copy/rename 단계에서 감지된다.
        let _ = (worktree, min_bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

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
    fn test_verify_backup_magic_accepts_valid_header() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-backup.sql");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "-- PostgreSQL database dump").unwrap();
        writeln!(f, "-- (다른 내용)").unwrap();
        assert!(verify_backup_magic(&path).is_ok());
    }

    #[test]
    fn test_verify_backup_magic_rejects_invalid_header() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-backup.sql");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "-- Some other dump").unwrap();
        let result = verify_backup_magic(&path);
        assert!(matches!(result, Err(Error::BackupFailed(_))));
    }

    #[test]
    fn test_verify_backup_magic_rejects_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.sql");
        let result = verify_backup_magic(&path);
        assert!(matches!(result, Err(Error::BackupFailed(_))));
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
