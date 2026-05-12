//! Protected path detection for Claude Code permission gating.
//!
//! # 책임 범위
//!
//! Claude CLI가 파일 접근 권한을 요청할 때, 해당 경로가 보호되어야 하는지 판단한다.
//! 보호 기준: 5개 dot-dir 이름 (`.git`, `.claude`, `.vscode`, `.idea`, `.husky`)
//! 또는 cwd/additional_dirs 외부 경로.
//!
//! Prefix 출처: Claude Code docs /en/permissions

use std::path::{Path, PathBuf};

const PROTECTED_DIR_NAMES: &[&str] = &[
    ".git",
    ".claude",
    ".vscode",
    ".idea",
    ".husky",
];

/// 5 보호 dot-dir 이름 매칭 여부. cwd 검사는 안 함.
/// 안내 메시지 분기 용도.
///
/// component 단위로 매칭하므로 `/proj/.git` (디렉토리 자체) 와
/// `/proj/.git/HEAD` (자식) 모두 감지한다.
/// `.gitignore` 같은 유사 이름 가짜 매칭 없음.
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

    resolved.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|name| PROTECTED_DIR_NAMES.contains(&name))
            .unwrap_or(false)
    })
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

/// Claude tool input 에서 path-bearing 필드를 추출한다.
/// tool 별로 키가 다름 — Edit/Write/Read 는 `file_path`, NotebookEdit 는 `notebook_path`.
/// 알 수 없는 도구는 보수적으로 `file_path` → `notebook_path` → `path` 순으로 fallback.
pub fn permission_target_path(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    let primary_key = match tool_name {
        "NotebookEdit" => "notebook_path",
        _ => "file_path",
    };
    input
        .get(primary_key)
        .and_then(|v| v.as_str())
        .or_else(|| input.get("file_path").and_then(|v| v.as_str()))
        .or_else(|| input.get("notebook_path").and_then(|v| v.as_str()))
        .or_else(|| input.get("path").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
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

    // ── s1: component 단위 매칭 신규 테스트 ──────────────────────────────────

    #[test]
    fn is_in_protected_prefix_dotgit_exact_dir_returns_true() {
        assert!(is_in_protected_prefix(Some("/proj/.git")));
    }

    #[test]
    fn is_in_protected_prefix_dotclaude_exact_dir_returns_true() {
        assert!(is_in_protected_prefix(Some("/proj/.claude")));
    }

    #[test]
    fn is_in_protected_prefix_dotvscode_exact_dir_returns_true() {
        assert!(is_in_protected_prefix(Some("/proj/.vscode")));
    }

    #[test]
    fn is_in_protected_prefix_dotidea_exact_dir_returns_true() {
        assert!(is_in_protected_prefix(Some("/proj/.idea")));
    }

    #[test]
    fn is_in_protected_prefix_dothusky_exact_dir_returns_true() {
        assert!(is_in_protected_prefix(Some("/proj/.husky")));
    }

    #[test]
    fn is_in_protected_prefix_gitignore_does_not_match() {
        // `.gitignore` 는 보호 대상 아님 — component 매칭이라 substring 가짜 매칭 없음
        assert!(!is_in_protected_prefix(Some("/proj/.gitignore")));
    }

    // ── w2: permission_target_path 신규 테스트 ───────────────────────────────

    #[test]
    fn permission_target_path_edit_uses_file_path() {
        let input = serde_json::json!({"file_path": "/x"});
        assert_eq!(permission_target_path("Edit", &input), Some("/x".to_string()));
    }

    #[test]
    fn permission_target_path_notebook_uses_notebook_path() {
        let input = serde_json::json!({"notebook_path": "/y.ipynb"});
        assert_eq!(
            permission_target_path("NotebookEdit", &input),
            Some("/y.ipynb".to_string())
        );
    }

    #[test]
    fn permission_target_path_bash_returns_none() {
        let input = serde_json::json!({"command": "ls"});
        assert_eq!(permission_target_path("Bash", &input), None);
    }
}
