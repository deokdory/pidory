//! Path resolution for Claude settings files.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use super::error::ClaudeSettingsError;

/// Resolves and canonicalizes a settings file path.
///
/// Steps:
/// 1. `~` expansion via `HOME` env var (systemd sets this correctly).
/// 2. Ensures parent directory exists (`ensure_parent_dir`).
/// 3. Canonicalizes the parent directory (resolves symlinks, makes absolute).
/// 4. Joins the filename back onto the canonical parent.
/// 5. Checks for symlink (if target file exists).
/// 6. Checks for directory (if target path is a dir).
///
/// # Errors
///
/// - `InvalidPath` — HOME not set, no parent dir, or path has no filename component.
/// - `SymlinkNotSupported` — target exists and is a symlink.
/// - `IsADirectory` — target exists and is a directory.
/// - `Io` — underlying I/O error.
#[allow(dead_code)]
pub(crate) fn canonical_settings_path(input: &Path) -> Result<PathBuf, ClaudeSettingsError> {
    // Step 1: ~ expansion
    let expanded = expand_tilde(input)?;

    // Step 2: ensure parent directory exists (mkdir -p)
    ensure_parent_dir(&expanded)?;

    // Step 3: extract parent and filename
    let parent = expanded.parent().ok_or_else(|| ClaudeSettingsError::InvalidPath {
        path: expanded.clone(),
        reason: "no parent dir".to_string(),
    })?;

    let filename = expanded.file_name().ok_or_else(|| ClaudeSettingsError::InvalidPath {
        path: expanded.clone(),
        reason: "path has no filename component".to_string(),
    })?;

    // Step 4: canonicalize the (now-existing) parent
    let canonical_parent = fs::canonicalize(parent)?;

    // Step 5: join filename onto canonical parent
    let result = canonical_parent.join(filename);

    // Step 6: symlink check — only if file exists
    if result.exists() || fs::symlink_metadata(&result).is_ok() {
        let meta = fs::symlink_metadata(&result)?;
        if meta.file_type().is_symlink() {
            return Err(ClaudeSettingsError::SymlinkNotSupported { path: result });
        }
        // Step 7: directory check
        if meta.is_dir() {
            return Err(ClaudeSettingsError::IsADirectory { path: result });
        }
    }

    Ok(result)
}

/// Ensures the parent directory of `path` exists, creating it with mode 0755 if needed.
///
/// # Errors
///
/// - `InvalidPath` — `path` has no parent component.
/// - `Io` — underlying I/O error (e.g. permission denied).
#[allow(dead_code)]
pub(crate) fn ensure_parent_dir(path: &Path) -> Result<(), ClaudeSettingsError> {
    let parent = path.parent().ok_or_else(|| ClaudeSettingsError::InvalidPath {
        path: path.to_path_buf(),
        reason: "no parent dir".to_string(),
    })?;

    if parent.as_os_str().is_empty() {
        // Relative path with no dir component — treat as current dir, always exists.
        return Ok(());
    }

    if !parent.exists() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(parent)?;
    }

    Ok(())
}

/// Expands a leading `~` using the `HOME` environment variable.
///
/// If the path does not start with `~`, it is returned unchanged (as a `PathBuf`).
#[allow(dead_code)]
fn expand_tilde(path: &Path) -> Result<PathBuf, ClaudeSettingsError> {
    let path_str = path.to_string_lossy();

    if path_str.starts_with("~/") || path_str == "~" {
        let home = std::env::var("HOME").map_err(|_| ClaudeSettingsError::InvalidPath {
            path: path.to_path_buf(),
            reason: "HOME env var not set".to_string(),
        })?;

        if home.is_empty() {
            return Err(ClaudeSettingsError::InvalidPath {
                path: path.to_path_buf(),
                reason: "HOME env var not set".to_string(),
            });
        }

        let without_tilde = path_str.strip_prefix("~/").unwrap_or("");
        let expanded = if without_tilde.is_empty() {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(without_tilde)
        };

        Ok(expanded)
    } else {
        Ok(path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Test 1: `~/.claude/settings.json` → absolute path with HOME expansion.
    #[test]
    fn tilde_expansion_produces_absolute_path() {
        // HOME must be set in test environment.
        let home = std::env::var("HOME").expect("HOME must be set in test env");
        let input = Path::new("~/.claude/settings.json");
        let result = canonical_settings_path(input).expect("should succeed");
        let expected_suffix = ".claude/settings.json";
        assert!(result.is_absolute(), "result must be absolute: {result:?}");
        assert!(
            result.to_string_lossy().ends_with(expected_suffix),
            "expected path ending in {expected_suffix}, got: {result:?}"
        );
        assert!(
            result.to_string_lossy().starts_with(&home),
            "expected path under HOME={home}, got: {result:?}"
        );
    }

    /// Test 2: deep non-existent parent dirs are auto-created.
    #[test]
    fn missing_parent_dirs_are_created() {
        let tmp = TempDir::new().unwrap();
        let deep_path = tmp.path().join("x").join("y").join("z").join("settings.json");
        assert!(!deep_path.parent().unwrap().exists(), "precondition: x/y/z should not exist");

        let result = canonical_settings_path(&deep_path).expect("should succeed");

        assert!(result.is_absolute(), "result must be absolute");
        assert!(
            deep_path.parent().unwrap().exists(),
            "x/y/z directory should have been created"
        );
        assert_eq!(result.file_name().unwrap(), "settings.json");
    }

    /// Test 3: symlink target → `SymlinkNotSupported` error.
    #[test]
    fn symlink_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let real_target = tmp.path().join("real_target.json");
        fs::write(&real_target, b"{}").unwrap();

        let link_path = tmp.path().join("settings.json");
        std::os::unix::fs::symlink(&real_target, &link_path).unwrap();

        let err = canonical_settings_path(&link_path)
            .expect_err("symlink should be rejected");
        assert!(
            matches!(err, ClaudeSettingsError::SymlinkNotSupported { .. }),
            "expected SymlinkNotSupported, got: {err:?}"
        );
    }

    /// Test 4: path that is already a directory → `IsADirectory` error.
    #[test]
    fn directory_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let dir_path = tmp.path().join("settings.json");
        fs::create_dir(&dir_path).unwrap();

        let err = canonical_settings_path(&dir_path)
            .expect_err("directory should be rejected");
        assert!(
            matches!(err, ClaudeSettingsError::IsADirectory { .. }),
            "expected IsADirectory, got: {err:?}"
        );
    }
}
