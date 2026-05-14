pub mod backup;
pub mod build;
pub mod db_url;
pub mod git;
pub mod lock;
pub mod marker;
pub mod preflight;
pub mod restart;
pub mod skills;
pub mod version;
pub mod worktree;

use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum Error {
    #[error("working tree has uncommitted changes")]
    DirtyTree,
    #[error("git fetch failed: {0}")]
    FetchFailed(String),
    #[error("cargo build failed:\n{stderr_tail}")]
    BuildFailed { stderr_tail: String },
    #[error("skill sync failed: {0}")]
    SkillSyncFailed(String),
    #[error("update lock is held by pid {0}")]
    LockHeld(u32),
    #[error("already at the latest version")]
    AlreadyLatest,
    #[error("backup failed: {0}")]
    BackupFailed(String),
    #[error("restart failed: {0}")]
    RestartFailed(String),
    #[error("worktree not found")]
    WorktreeNotFound,
    #[error("path is not a pidory worktree")]
    NotPidoryWorktree,
    #[error("git index is locked (.git/index.lock exists)")]
    GitLocked,
    #[error("insufficient disk space for update")]
    InsufficientDiskSpace,
    #[error("active turns in threads: {}", _0.join(", "))]
    ActiveTurns(Vec<String>),
}
