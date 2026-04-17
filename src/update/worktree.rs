use std::path::{Path, PathBuf};
use std::process::Command;

use super::Error;

pub fn detect_worktree() -> Result<PathBuf, Error> {
    // 1. current_exe 부모 디렉토리부터 최대 10단계 거슬러 올라가며 탐색
    if let Ok(exe) = std::env::current_exe() {
        let mut current = exe.as_path().to_path_buf();
        for _ in 0..10 {
            if let Some(parent) = current.parent().map(|p| p.to_path_buf()) {
                if sanity_check(&parent).is_ok() {
                    return Ok(parent);
                }
                current = parent;
            } else {
                break;
            }
        }
    }

    // 2. fallback: CARGO_MANIFEST_DIR
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    if sanity_check(manifest_dir).is_ok() {
        return Ok(manifest_dir.to_path_buf());
    }

    Err(Error::WorktreeNotFound)
}

pub fn sanity_check(path: &Path) -> Result<(), Error> {
    // Cargo.toml에 name = "pidory" 라인 존재 확인
    let cargo_toml = path.join("Cargo.toml");
    if !cargo_toml.exists() {
        return Err(Error::NotPidoryWorktree);
    }

    let contents = std::fs::read_to_string(&cargo_toml)
        .map_err(|_| Error::NotPidoryWorktree)?;

    // 공백 허용 매칭: `name="pidory"`, `name =  "pidory"` 등 모두 매칭.
    let has_name = contents.lines().any(|line| {
        let normalized: String = line
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("")
            .to_string();
        normalized == r#"name="pidory""#
    });

    if !has_name {
        return Err(Error::NotPidoryWorktree);
    }

    // .git 존재 확인 (디렉토리 or 파일 — git worktree는 파일)
    let git_path = path.join(".git");
    if !git_path.exists() {
        return Err(Error::NotPidoryWorktree);
    }

    Ok(())
}

pub fn is_dirty(path: &Path) -> Result<bool, Error> {
    // git status --porcelain --untracked-files=normal로 modified/staged/untracked 전부 감지.
    // 비어있으면 clean, 아니면 dirty.
    let output = Command::new("git")
        .args([
            "-C",
            &path.to_string_lossy(),
            "status",
            "--porcelain",
            "--untracked-files=normal",
        ])
        .output()
        .map_err(|e| Error::FetchFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(Error::FetchFailed(stderr));
    }

    Ok(!output.stdout.is_empty())
}

pub fn current_commit(path: &Path) -> Result<String, Error> {
    let output = Command::new("git")
        .args(["-C", &path.to_string_lossy(), "rev-parse", "--short", "HEAD"])
        .output()
        .map_err(|e| Error::FetchFailed(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(Error::FetchFailed(stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn current_branch_or_tag(path: &Path) -> Result<String, Error> {
    // 먼저 태그 체크아웃인지 확인 (detached HEAD + 태그)
    let tag_output = Command::new("git")
        .args([
            "-C",
            &path.to_string_lossy(),
            "describe",
            "--tags",
            "--exact-match",
            "HEAD",
        ])
        .output()
        .map_err(|e| Error::FetchFailed(e.to_string()))?;

    if tag_output.status.success() {
        return Ok(String::from_utf8_lossy(&tag_output.stdout)
            .trim()
            .to_string());
    }

    // 실패 시 브랜치 이름
    let branch_output = Command::new("git")
        .args([
            "-C",
            &path.to_string_lossy(),
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
        ])
        .output()
        .map_err(|e| Error::FetchFailed(e.to_string()))?;

    if !branch_output.status.success() {
        let stderr = String::from_utf8_lossy(&branch_output.stderr).to_string();
        return Err(Error::FetchFailed(stderr));
    }

    Ok(String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// 테스트용 git repo 생성: git init + 빈 커밋
    fn make_test_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .expect("git init failed");

        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .expect("git config email failed");

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir)
            .output()
            .expect("git config name failed");

        // 빈 커밋 (--allow-empty)
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir)
            .output()
            .expect("git commit failed");
    }

    /// pidory Cargo.toml 생성 헬퍼
    fn write_pidory_cargo_toml(dir: &Path) {
        let content = "[package]\nname = \"pidory\"\nversion = \"0.0.0\"\nedition = \"2021\"\n";
        fs::write(dir.join("Cargo.toml"), content).expect("write Cargo.toml failed");
    }

    // ──────────────────────────────────────────────
    // sanity_check 테스트
    // ──────────────────────────────────────────────

    #[test]
    fn sanity_check_ok() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        write_pidory_cargo_toml(path);
        fs::create_dir(path.join(".git")).expect("mkdir .git");

        assert!(sanity_check(path).is_ok());
    }

    #[test]
    fn sanity_check_wrong_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        let content = "[package]\nname = \"other\"\nversion = \"0.0.0\"\n";
        fs::write(path.join("Cargo.toml"), content).expect("write");
        fs::create_dir(path.join(".git")).expect("mkdir .git");

        assert!(matches!(
            sanity_check(path),
            Err(Error::NotPidoryWorktree)
        ));
    }

    #[test]
    fn sanity_check_no_cargo_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        fs::create_dir(path.join(".git")).expect("mkdir .git");

        assert!(matches!(
            sanity_check(path),
            Err(Error::NotPidoryWorktree)
        ));
    }

    #[test]
    fn sanity_check_tmp() {
        let path = Path::new("/tmp");
        assert!(matches!(
            sanity_check(path),
            Err(Error::NotPidoryWorktree)
        ));
    }

    #[test]
    fn sanity_check_no_git_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        write_pidory_cargo_toml(path);
        // .git 없음

        assert!(matches!(
            sanity_check(path),
            Err(Error::NotPidoryWorktree)
        ));
    }

    // ──────────────────────────────────────────────
    // is_dirty / current_commit 테스트
    // ──────────────────────────────────────────────

    #[test]
    fn is_dirty_clean_repo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        make_test_repo(path);

        let dirty = is_dirty(path).expect("is_dirty");
        assert!(!dirty, "빈 커밋 직후 — clean 이어야 함");
    }

    #[test]
    fn is_dirty_with_staged_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        make_test_repo(path);

        // 파일 추가 후 stage
        fs::write(path.join("dirty.txt"), "hello").expect("write");
        Command::new("git")
            .args(["add", "dirty.txt"])
            .current_dir(path)
            .output()
            .expect("git add");

        let dirty = is_dirty(path).expect("is_dirty");
        assert!(dirty, "staged 파일이 있으므로 dirty 이어야 함");
    }

    #[test]
    fn is_dirty_with_untracked_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        make_test_repo(path);

        // untracked 파일 하나만 추가 (add/commit 없음)
        fs::write(path.join("untracked.txt"), "hello").expect("write");

        let dirty = is_dirty(path).expect("is_dirty");
        assert!(dirty, "untracked 파일이 있으므로 dirty 이어야 함");
    }

    #[test]
    fn sanity_check_accepts_spaced_name_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        // 탭·비표준 공백 배치
        let content = "[package]\nname\t=   \"pidory\"\nversion = \"0.0.0\"\n";
        fs::write(path.join("Cargo.toml"), content).expect("write");
        fs::create_dir(path.join(".git")).expect("mkdir .git");

        assert!(sanity_check(path).is_ok());
    }

    #[test]
    fn is_dirty_with_modified_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        make_test_repo(path);

        // 파일 커밋 후 수정
        fs::write(path.join("file.txt"), "original").expect("write");
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(path)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "add file"])
            .current_dir(path)
            .output()
            .expect("git commit");

        fs::write(path.join("file.txt"), "modified").expect("write modified");

        let dirty = is_dirty(path).expect("is_dirty");
        assert!(dirty, "수정된 파일이 있으므로 dirty 이어야 함");
    }

    #[test]
    fn current_commit_returns_short_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        make_test_repo(path);

        let commit = current_commit(path).expect("current_commit");
        // short hash: 7자 이상, hex만
        assert!(!commit.is_empty());
        assert!(commit.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(commit.len() <= 12, "short hash 길이 초과: {commit}");
    }

    #[test]
    fn current_branch_returns_branch_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path();

        make_test_repo(path);

        let branch = current_branch_or_tag(path).expect("current_branch_or_tag");
        // 기본 브랜치: main 또는 master
        assert!(
            branch == "main" || branch == "master",
            "예상치 못한 브랜치: {branch}"
        );
    }
}
