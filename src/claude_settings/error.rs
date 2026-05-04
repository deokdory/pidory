//! Error types for the claude_settings module.

use std::path::PathBuf;
use std::time::Duration;

#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum ClaudeSettingsError {
    /// Generic file I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The path exists but the process lacks write permission.
    #[error("permission denied: {path}")]
    PermissionDenied { path: PathBuf },

    /// The path is a directory, not a file.
    #[error("path is a directory: {path}")]
    IsADirectory { path: PathBuf },

    /// Symlinks are not supported.
    #[error("symlink not supported: {path}")]
    SymlinkNotSupported { path: PathBuf },

    /// File exceeds the size limit.
    #[error("file too large: {path} ({size} bytes, limit {limit} bytes)")]
    FileTooLarge { path: PathBuf, size: u64, limit: u64 },

    /// JSON parse failed; a backup was saved at `backup`.
    #[error("JSON corrupted: {path} (backup saved at {backup})")]
    JsonCorrupted {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
        backup: PathBuf,
    },

    /// A single lock-attempt timed out after `waited`.
    #[error("lock timeout after {waited:?}: {path}")]
    LockTimeout { path: PathBuf, waited: Duration },

    /// All lock attempts (5 s + 1 retry) failed.
    #[error("lock conflict — could not acquire lock: {path}")]
    LockConflict { path: PathBuf },

    /// File was modified by another writer during the read-modify-write cycle.
    #[error("mtime changed during read-modify-write: {path}")]
    MtimeChangedDuringRmw { path: PathBuf },

    /// The parent directory is not writable.
    #[error("parent directory not writable: {path}")]
    ParentDirNotWritable { path: PathBuf },

    /// The supplied path is invalid for the given reason.
    #[error("invalid path {path}: {reason}")]
    InvalidPath { path: PathBuf, reason: String },

    /// 기존 settings JSON의 형태가 mutator 기대와 다름 (root가 object 아님,
    /// `permissions`가 object 아님, `permissions.allow`가 array 아님 등).
    /// review #295 w2 — 이전에는 silent no-op + `Added` 반환이었음.
    #[error("invalid settings shape at {path}: {reason}")]
    InvalidShape { path: PathBuf, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn symlink_not_supported_display_contains_path() {
        let err = ClaudeSettingsError::SymlinkNotSupported {
            path: PathBuf::from("/tmp/foo"),
        };
        let msg = err.to_string();
        assert!(msg.contains("/tmp/foo"), "expected path in message, got: {msg}");
    }

    #[test]
    fn from_io_error_produces_io_variant() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        let err: ClaudeSettingsError = io_err.into();
        assert!(matches!(err, ClaudeSettingsError::Io(_)));
    }

    #[test]
    fn json_corrupted_source_is_some() {
        let serde_err = serde_json::from_str::<serde_json::Value>("{invalid")
            .unwrap_err();
        let err = ClaudeSettingsError::JsonCorrupted {
            path: PathBuf::from("/tmp/settings.json"),
            source: serde_err,
            backup: PathBuf::from("/tmp/settings.json.corrupted-1234"),
        };
        assert!(err.source().is_some());
    }
}
