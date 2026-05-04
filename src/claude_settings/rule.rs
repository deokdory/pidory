//! Rule domain types: RuleKind, Scope, and path resolution.
//!
//! # 책임 범위
//!
//! - [`RuleKind`] — Claude settings.json permission rule 매칭 방식
//! - [`Scope`] — Project-local vs Global 적용 범위
//! - [`scope_to_path`] — Scope → settings 파일 경로 변환
//! - [`available_rule_kinds`] / [`build_rule_text`] — RuleBuilder 구현

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Claude permission rule 매칭 방식.
///
/// `settings.json` / `settings.local.json` `permissions.allow` 배열 엔트리 형태를 결정한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleKind {
    /// 정확히 일치하는 rule (e.g. `Bash(npm install)`)
    Exact,
    /// prefix wildcard rule (e.g. `Bash(npm *)`)
    Prefix,
    /// domain 단위 허용 (e.g. `WebFetch(domain:example.com)`)
    Domain,
    /// tool 전체 허용 (e.g. `Bash(*)`)
    Tool,
}

/// Permission rule이 적용되는 파일 범위.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// `.claude/settings.local.json` — 프로젝트 로컬
    Project,
    /// `~/.claude/settings.json` — 전역
    Global,
}

impl Scope {
    /// Project ↔ Global 토글.
    pub fn flip(self) -> Self {
        match self {
            Scope::Project => Scope::Global,
            Scope::Global => Scope::Project,
        }
    }
}

/// AlwaysAllow 의 default scope. P1.3 (#288) 에서 DB user_settings 조회로 교체.
pub fn default_scope() -> Scope {
    Scope::Project
}

/// Scope와 경로 정보로 settings 파일 절대 경로를 반환한다.
///
/// - `Scope::Project` → `{project_root}/.claude/settings.local.json`
/// - `Scope::Global`  → `{home}/.claude/settings.json`
pub fn scope_to_path(scope: Scope, project_root: &Path, home: &Path) -> PathBuf {
    match scope {
        Scope::Project => project_root.join(".claude").join("settings.local.json"),
        Scope::Global => home.join(".claude").join("settings.json"),
    }
}

/// 주어진 tool + input에 대해 선택 가능한 RuleKind 목록을 반환한다.
///
/// Unknown tool 은 `Tool` 만 반환한다 — input schema 를 모르므로 Exact 의 첫
/// string 필드 fallback 은 비결정적이라 위험하다 (review #297 s2 fix).
pub fn available_rule_kinds(tool: &str, input: &serde_json::Value) -> Vec<RuleKind> {
    match tool {
        "Bash" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if cmd.is_empty() {
                vec![RuleKind::Tool]
            } else {
                vec![RuleKind::Exact, RuleKind::Prefix, RuleKind::Tool]
            }
        }
        "WebFetch" => {
            let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
            match parse_host(url) {
                Some(host) if !is_ip(&host) => vec![RuleKind::Domain, RuleKind::Tool],
                _ => vec![RuleKind::Tool],
            }
        }
        "Read" | "Edit" | "Write" => vec![RuleKind::Exact, RuleKind::Tool],
        "Grep" | "Glob" => vec![RuleKind::Tool],
        _ => vec![RuleKind::Tool],
    }
}

/// 주어진 tool + input + kind로 settings.json에 삽입할 rule 문자열을 생성한다.
///
/// `RuleKind::Tool` 은 항상 `<Tool>(*)` canonical form 으로 직렬화한다 — bare
/// tool name (e.g. `"Bash"`) 은 Claude permission spec 에 정의되어 있지 않다
/// (review #297 w2 fix, memory: reference_claude_permission_rule_syntax).
pub fn build_rule_text(tool: &str, input: &serde_json::Value, kind: RuleKind) -> Option<String> {
    // RuleKind::Tool: 항상 <Tool>(*) canonical form
    if matches!(kind, RuleKind::Tool) {
        return Some(format!("{}(*)", tool));
    }
    match (tool, kind) {
        ("Bash", RuleKind::Exact) => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if cmd.is_empty() {
                None
            } else {
                Some(format!("Bash({})", cmd))
            }
        }
        ("Bash", RuleKind::Prefix) => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let first_token = cmd.split_whitespace().next()?;
            Some(format!("Bash({} *)", first_token))
        }
        ("WebFetch", RuleKind::Domain) => {
            let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let host = parse_host(url)?;
            if is_ip(&host) {
                None
            } else {
                Some(format!("WebFetch(domain:{})", host))
            }
        }
        // WebFetch 는 Domain 만 지원 (Tool 은 위에서 처리)
        ("WebFetch", _) => None,
        ("Read", RuleKind::Exact) => {
            let file_path = input.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
            if file_path.is_empty() {
                None
            } else {
                Some(format!("Read({})", file_path))
            }
        }
        ("Edit", RuleKind::Exact) => {
            let file_path = input.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
            if file_path.is_empty() {
                None
            } else {
                Some(format!("Edit({})", file_path))
            }
        }
        ("Write", RuleKind::Exact) => {
            let file_path = input.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
            if file_path.is_empty() {
                None
            } else {
                Some(format!("Write({})", file_path))
            }
        }
        // Unknown tool: Exact 비활성화 (input schema 를 모르므로 비결정적)
        _ => None,
    }
}

/// URL 문자열에서 host를 추출한다 (url crate 없이 단순 파싱).
fn parse_host(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://")?.1;
    let host_and_rest = after_scheme.split(['/', '?', '#']).next()?;
    // strip user:password@
    let host_and_port = host_and_rest
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(host_and_rest);
    // strip :port (단, IPv6 [...]는 보존)
    let host = if host_and_port.starts_with('[') {
        host_and_port.split_once(']')?.0.trim_start_matches('[').to_string()
    } else {
        host_and_port.split(':').next()?.to_string()
    };
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// host 문자열이 IP 주소(IPv4 / IPv6)인지 확인한다.
fn is_ip(host: &str) -> bool {
    IpAddr::from_str(host).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scope_to_path_project() {
        let path = scope_to_path(
            Scope::Project,
            Path::new("/proj"),
            Path::new("/home/u"),
        );
        assert_eq!(path, PathBuf::from("/proj/.claude/settings.local.json"));
    }

    #[test]
    fn scope_to_path_global() {
        let path = scope_to_path(
            Scope::Global,
            Path::new("/proj"),
            Path::new("/home/u"),
        );
        assert_eq!(path, PathBuf::from("/home/u/.claude/settings.json"));
    }

    // available_rule_kinds 테스트

    #[test]
    fn available_kinds_bash_with_command() {
        let kinds = available_rule_kinds("Bash", &json!({"command": "npm test"}));
        assert_eq!(kinds, vec![RuleKind::Exact, RuleKind::Prefix, RuleKind::Tool]);
    }

    #[test]
    fn available_kinds_bash_empty_command() {
        let kinds = available_rule_kinds("Bash", &json!({"command": ""}));
        assert_eq!(kinds, vec![RuleKind::Tool]);
    }

    #[test]
    fn available_kinds_webfetch_domain() {
        let kinds = available_rule_kinds("WebFetch", &json!({"url": "https://example.com/api"}));
        assert_eq!(kinds, vec![RuleKind::Domain, RuleKind::Tool]);
    }

    #[test]
    fn available_kinds_webfetch_ip() {
        let kinds = available_rule_kinds("WebFetch", &json!({"url": "http://192.168.1.1/api"}));
        assert_eq!(kinds, vec![RuleKind::Tool]);
    }

    #[test]
    fn available_kinds_read_empty_input() {
        let kinds = available_rule_kinds("Read", &json!({}));
        assert_eq!(kinds, vec![RuleKind::Exact, RuleKind::Tool]);
    }

    #[test]
    fn available_kinds_grep_empty_input() {
        let kinds = available_rule_kinds("Grep", &json!({}));
        assert_eq!(kinds, vec![RuleKind::Tool]);
    }

    #[test]
    fn available_kinds_unknown_tool() {
        // Unknown tool: input schema 모르므로 Tool 만 (review #297 s2 fix)
        let kinds = available_rule_kinds("Unknown", &json!({}));
        assert_eq!(kinds, vec![RuleKind::Tool]);
    }

    #[test]
    fn default_scope_is_project() {
        assert_eq!(default_scope(), Scope::Project);
    }

    // build_rule_text 테스트

    #[test]
    fn build_bash_exact() {
        let result = build_rule_text("Bash", &json!({"command": "npm test"}), RuleKind::Exact);
        assert_eq!(result, Some("Bash(npm test)".to_string()));
    }

    #[test]
    fn build_bash_prefix() {
        let result = build_rule_text("Bash", &json!({"command": "npm test --watch"}), RuleKind::Prefix);
        assert_eq!(result, Some("Bash(npm *)".to_string()));
    }

    #[test]
    fn build_bash_tool() {
        // Tool 은 항상 <Tool>(*) canonical form (review #297 w2 fix)
        let result = build_rule_text("Bash", &json!({}), RuleKind::Tool);
        assert_eq!(result, Some("Bash(*)".to_string()));
    }

    #[test]
    fn build_webfetch_domain() {
        let result = build_rule_text("WebFetch", &json!({"url": "https://api.example.com/v1"}), RuleKind::Domain);
        assert_eq!(result, Some("WebFetch(domain:api.example.com)".to_string()));
    }

    #[test]
    fn build_webfetch_tool() {
        let result = build_rule_text("WebFetch", &json!({}), RuleKind::Tool);
        assert_eq!(result, Some("WebFetch(*)".to_string()));
    }

    #[test]
    fn build_read_exact() {
        let result = build_rule_text("Read", &json!({"file_path": "/etc/hosts"}), RuleKind::Exact);
        assert_eq!(result, Some("Read(/etc/hosts)".to_string()));
    }

    #[test]
    fn build_grep_tool() {
        let result = build_rule_text("Grep", &json!({}), RuleKind::Tool);
        assert_eq!(result, Some("Grep(*)".to_string()));
    }

    #[test]
    fn build_unknown_tool_exact_returns_none() {
        // Unknown tool: Exact 비활성화 (review #297 s2 fix)
        let result = build_rule_text("MyTool", &json!({"x": "y"}), RuleKind::Exact);
        assert_eq!(result, None);
    }

    #[test]
    fn build_unknown_tool_tool_form() {
        // Unknown tool 도 Tool 은 항상 가능
        let result = build_rule_text("MyTool", &json!({}), RuleKind::Tool);
        assert_eq!(result, Some("MyTool(*)".to_string()));
    }

    #[test]
    fn build_webfetch_exact_mismatch() {
        let result = build_rule_text("WebFetch", &json!({"url": "https://example.com"}), RuleKind::Exact);
        assert_eq!(result, None);
    }

    #[test]
    fn build_bash_domain_mismatch() {
        let result = build_rule_text("Bash", &json!({"command": "ls"}), RuleKind::Domain);
        assert_eq!(result, None);
    }
}
