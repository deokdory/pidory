//! Protected path detection for Claude Code permission gating.
//!
//! # 책임 범위
//!
//! Claude CLI가 파일 접근 권한을 요청할 때, 해당 경로가 보호되어야 하는지 판단한다.
//! 보호 기준: 5개 dot-dir prefix (`.git/`, `.claude/`, `.vscode/`, `.idea/`, `.husky/`)
//! 또는 cwd/additional_dirs 외부 경로.
//!
//! Prefix 출처: Claude Code docs /en/permissions

use std::path::{Path, PathBuf};

const PROTECTED_PREFIXES: &[&str] = &[
    "/.git/",
    "/.claude/",
    "/.vscode/",
    "/.idea/",
    "/.husky/",
];

/// 5 보호 prefix 매칭 여부. cwd 검사는 안 함.
/// 안내 메시지 분기 용도.
///
/// `file_path = None` → `false`
pub fn is_in_protected_prefix(file_path: Option<&str>) -> bool {
    let raw = match file_path {
        None => return false,
        Some(p) => p,
    };

    let resolved: PathBuf = match std::fs::canonicalize(raw) {
        Ok(canonical) => canonical,
        Err(_) => PathBuf::from(raw),
    };

    let resolved_str = resolved.to_string_lossy();

    for prefix in PROTECTED_PREFIXES {
        if resolved_str.contains(prefix) {
            return true;
        }
    }

    false
}

/// cwd + additional_dirs 외부 여부. 보호 prefix 검사는 안 함.
///
/// `file_path = None` → `false`
pub fn is_outside_workspace(
    file_path: Option<&str>,
    cwd: &Path,
    additional_dirs: &[PathBuf],
) -> bool {
    let raw = match file_path {
        None => return false,
        Some(p) => p,
    };

    let resolved: PathBuf = match std::fs::canonicalize(raw) {
        Ok(canonical) => canonical,
        Err(_) => PathBuf::from(raw),
    };

    let in_cwd = resolved.starts_with(cwd);
    let in_additional = additional_dirs.iter().any(|dir| resolved.starts_with(dir));

    !in_cwd && !in_additional
}

/// 주어진 파일 경로가 보호되어야 하는지 판단한다.
///
/// 보호 조건 (OR):
/// 1. `file_path`가 `None` → `false` (Bash 등 파일 경로 없는 도구)
/// 2. 경로 문자열에 `PROTECTED_PREFIXES` 중 하나라도 포함 → `true`
/// 3. `cwd` 및 `additional_dirs` 모두에 `starts_with` 매칭 안 되면 (외부 경로) → `true`
/// 4. 그 외 → `false`
pub fn is_protected_path(
    file_path: Option<&str>,
    cwd: &Path,
    additional_dirs: &[PathBuf],
) -> bool {
    is_in_protected_prefix(file_path) || is_outside_workspace(file_path, cwd, additional_dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_protected_path_dotclaude() {
        let path = "/proj/.claude/STATE.md";
        let cwd = Path::new("/proj");
        assert!(is_protected_path(Some(path), cwd, &[]));
    }

    #[test]
    fn is_protected_path_dotgit() {
        let path = "/proj/.git/HEAD";
        let cwd = Path::new("/proj");
        assert!(is_protected_path(Some(path), cwd, &[]));
    }

    #[test]
    fn is_protected_path_outside_cwd() {
        let path = "/tmp/x.md";
        let cwd = Path::new("/proj");
        assert!(is_protected_path(Some(path), cwd, &[]));
    }

    #[test]
    fn is_protected_path_in_additional_dir() {
        let path = "/tmp/canary.md";
        let cwd = Path::new("/proj");
        let additional = vec![PathBuf::from("/tmp")];
        assert!(!is_protected_path(Some(path), cwd, &additional));
    }

    #[test]
    fn is_protected_path_none_returns_false() {
        let cwd = Path::new("/any");
        assert!(!is_protected_path(None, cwd, &[]));
    }

    #[test]
    fn is_in_protected_prefix_dotgit_returns_true() {
        let path = "/proj/.git/HEAD";
        assert!(is_in_protected_prefix(Some(path)));
    }

    #[test]
    fn is_in_protected_prefix_none_returns_false() {
        assert!(!is_in_protected_prefix(None));
    }

    #[test]
    fn is_outside_workspace_returns_true() {
        let path = "/tmp/x.md";
        let cwd = Path::new("/proj");
        assert!(is_outside_workspace(Some(path), cwd, &[]));
    }

    #[test]
    fn is_outside_workspace_inside_cwd_returns_false() {
        let path = "/proj/src/main.rs";
        let cwd = Path::new("/proj");
        assert!(!is_outside_workspace(Some(path), cwd, &[]));
    }
}
