//! Claude settings reader — 3-layer merge of `additionalDirectories`.
//!
//! Precedence (highest → lowest, all merged): Local → Shared → User.
//! All three layers are read and their `additionalDirectories` entries are
//! combined, sorted, and deduped so that every source is respected.
//!
//! Layer paths:
//! - User:   `~/.claude/settings.json`
//! - Shared: `<project>/.claude/settings.json`
//! - Local:  `<project>/.claude/settings.local.json`

use std::path::{Path, PathBuf};

/// Resolved settings collected from all three settings layers.
pub struct ResolvedSettings {
    /// Union of `additionalDirectories` from all three settings layers,
    /// sorted and deduplicated.
    pub additional_dirs: Vec<PathBuf>,
}

/// Read and merge settings from all three layers using the real `HOME`
/// environment variable.  Delegates to [`resolve_settings_from_paths`].
pub fn resolve_settings(project_path: &Path) -> ResolvedSettings {
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    resolve_settings_from_paths(home.as_deref(), project_path)
}

/// Inner implementation — accepts explicit `home` so tests can bypass the
/// `HOME` environment variable.
fn resolve_settings_from_paths(home: Option<&Path>, project: &Path) -> ResolvedSettings {
    let mut dirs: Vec<PathBuf> = Vec::new();

    // Layer 1 — User: ~/.claude/settings.json
    if let Some(h) = home {
        let user_settings = h.join(".claude").join("settings.json");
        read_additional_dirs(&user_settings, home, &mut dirs);
    }

    // Layer 2 — Shared: <project>/.claude/settings.json
    let shared_settings = project.join(".claude").join("settings.json");
    read_additional_dirs(&shared_settings, home, &mut dirs);

    // Layer 3 — Local: <project>/.claude/settings.local.json
    let local_settings = project.join(".claude").join("settings.local.json");
    read_additional_dirs(&local_settings, home, &mut dirs);

    dirs.sort();
    dirs.dedup();

    ResolvedSettings { additional_dirs: dirs }
}

/// Read a single settings file and append its `additionalDirectories` entries
/// into `dirs`.  Missing files and parse failures are silently skipped (with a
/// warning log).
fn read_additional_dirs(path: &Path, home: Option<&Path>, dirs: &mut Vec<PathBuf>) {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return, // file absent or unreadable — silent skip
    };
    extract_additional_dirs(&content, home, dirs);
}

/// Parse `additionalDirectories` out of a JSON string and push entries into
/// `dirs`.  On parse failure a warning is emitted and the function returns
/// without touching `dirs`.
fn extract_additional_dirs(json_content: &str, home: Option<&Path>, dirs: &mut Vec<PathBuf>) {
    let value: serde_json::Value = match serde_json::from_str(json_content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("settings_reader: failed to parse JSON: {}", e);
            return;
        }
    };

    let Some(array) = value.get("additionalDirectories").and_then(|v| v.as_array()) else {
        return;
    };

    for item in array {
        if let Some(s) = item.as_str() {
            dirs.push(expand_tilde(s, home));
        }
    }
}

/// Expand a leading `~` using the provided `home` directory.
///
/// - `"~"` → `home` (or raw `PathBuf::from("~")` when home is `None`)
/// - `"~/foo"` → `home/foo` (or raw `PathBuf::from("~/foo")` when home is `None`)
/// - anything else → `PathBuf::from(s)` unchanged
fn expand_tilde(s: &str, home: Option<&Path>) -> PathBuf {
    if s == "~" {
        home.map(|h| h.to_path_buf()).unwrap_or_else(|| PathBuf::from("~"))
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(s))
    } else {
        PathBuf::from(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn resolve_settings_merges_global_and_project() {
        let home_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();

        // User layer: ~/.claude/settings.json
        let user_claude = home_dir.path().join(".claude");
        fs::create_dir_all(&user_claude).unwrap();
        fs::write(
            user_claude.join("settings.json"),
            r#"{"additionalDirectories": ["/alpha", "/beta"]}"#,
        )
        .unwrap();

        // Shared layer: <project>/.claude/settings.json
        let proj_claude = project_dir.path().join(".claude");
        fs::create_dir_all(&proj_claude).unwrap();
        fs::write(
            proj_claude.join("settings.json"),
            r#"{"additionalDirectories": ["/beta", "/gamma"]}"#,
        )
        .unwrap();

        let result =
            resolve_settings_from_paths(Some(home_dir.path()), project_dir.path());

        // /beta appears in both layers but dedup removes the duplicate
        assert_eq!(
            result.additional_dirs,
            vec![
                PathBuf::from("/alpha"),
                PathBuf::from("/beta"),
                PathBuf::from("/gamma"),
            ]
        );
    }

    #[test]
    fn resolve_settings_expands_tilde() {
        let home_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();

        let user_claude = home_dir.path().join(".claude");
        fs::create_dir_all(&user_claude).unwrap();
        fs::write(
            user_claude.join("settings.json"),
            r#"{"additionalDirectories": ["~/scratch"]}"#,
        )
        .unwrap();

        let result =
            resolve_settings_from_paths(Some(home_dir.path()), project_dir.path());

        assert_eq!(
            result.additional_dirs,
            vec![home_dir.path().join("scratch")]
        );
    }

    #[test]
    fn resolve_settings_ignores_invalid_json() {
        let home_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();

        let user_claude = home_dir.path().join(".claude");
        fs::create_dir_all(&user_claude).unwrap();
        fs::write(user_claude.join("settings.json"), "{ broken").unwrap();

        let result =
            resolve_settings_from_paths(Some(home_dir.path()), project_dir.path());

        assert!(result.additional_dirs.is_empty());
    }
}
