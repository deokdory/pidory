use std::path::Path;
use tokio::process::Command;

use super::Error;

fn check_git_lock(worktree: &Path) -> Result<(), Error> {
    let lock_path = worktree.join(".git").join("index.lock");
    if lock_path.exists() {
        return Err(Error::GitLocked);
    }
    Ok(())
}

fn tail_stderr(stderr: &[u8], n_lines: usize) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n_lines);
    lines[start..].join("\n")
}

pub async fn fetch_tags(worktree: &Path) -> Result<(), Error> {
    check_git_lock(worktree)?;
    let output = Command::new("git")
        .args(["-C", &worktree.to_string_lossy(), "fetch", "--tags", "--prune"])
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| Error::FetchFailed(e.to_string()))?;
    if !output.status.success() {
        return Err(Error::FetchFailed(tail_stderr(&output.stderr, 50)));
    }
    Ok(())
}

pub async fn checkout_tag(worktree: &Path, tag: &str) -> Result<(), Error> {
    check_git_lock(worktree)?;
    let tag_ref = format!("refs/tags/{tag}");
    let output = Command::new("git")
        .args(["-C", &worktree.to_string_lossy(), "reset", "--hard", &tag_ref])
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| Error::FetchFailed(e.to_string()))?;
    if !output.status.success() {
        return Err(Error::FetchFailed(tail_stderr(&output.stderr, 50)));
    }
    Ok(())
}
