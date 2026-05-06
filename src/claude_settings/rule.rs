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
///
/// MCP tool (`mcp__` prefix) 은 괄호 없는 exact form 만 유효하므로 `Tool` 만 반환한다
/// (Claude Code permission spec MCP 항목 — `mcp__<server>__<tool>` 형식, 괄호 불가).
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
        "Skill" => {
            if skill_name(input).is_some() {
                vec![RuleKind::Exact, RuleKind::Tool]
            } else {
                vec![RuleKind::Tool]
            }
        }
        // MCP tool: `mcp__<server>__<tool>` 형식 — Tool 만 허용 (의도 명시)
        t if t.starts_with("mcp__") => vec![RuleKind::Tool],
        _ => vec![RuleKind::Tool],
    }
}

/// 주어진 tool + input + kind로 settings.json에 삽입할 rule 문자열을 생성한다.
///
/// `RuleKind::Tool` 은 항상 `<Tool>(*)` canonical form 으로 직렬화한다 — bare
/// tool name (e.g. `"Bash"`) 은 Claude permission spec 에 정의되어 있지 않다
/// (review #297 w2 fix, memory: reference_claude_permission_rule_syntax).
///
/// 단, MCP tool (`mcp__` prefix) 은 괄호 없는 exact form 을 반환한다
/// (Claude Code permission spec MCP 항목 — `mcp__<server>__<tool>` 형식, 괄호 사용 불가).
pub fn build_rule_text(tool: &str, input: &serde_json::Value, kind: RuleKind) -> Option<String> {
    // RuleKind::Tool: MCP tool 은 괄호 없는 exact form, 그 외는 <Tool>(*) canonical form
    if matches!(kind, RuleKind::Tool) {
        if tool.starts_with("mcp__") {
            return Some(tool.to_string());
        }
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
        ("Skill", RuleKind::Exact) => {
            let name = skill_name(input)?;
            Some(format!("Skill({})", name))
        }
        // Unknown tool: Exact 비활성화 (input schema 를 모르므로 비결정적)
        _ => None,
    }
}

/// 주어진 tool + input + kind로 settings.json에 삽입할 rule 문자열 목록을 생성한다.
///
/// `build_rule_text` (단수) 의 복수형. Bash + Exact/Prefix 케이스에서는
/// `split_bash_subcommands` 로 파이프라인을 분리해 각 sub-command 마다 별도의 rule을 생성한다.
///
/// # 반환 규칙
///
/// - `Bash` + `Exact` or `Prefix` — sub-command 별 rule 목록 (빈 command → `vec![]`)
/// - `Bash` + `Tool`              — `vec!["Bash(*)"]` (canonical form)
/// - 그 외 (WebFetch, Read, ...) — `build_rule_text` 위임, None → `vec![]`
pub fn build_rule_texts(tool: &str, input: &serde_json::Value, kind: RuleKind) -> Vec<String> {
    match (tool, &kind) {
        ("Bash", RuleKind::Tool) => vec!["Bash(*)".to_string()],
        ("Bash", RuleKind::Exact) | ("Bash", RuleKind::Prefix) => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            if cmd.is_empty() {
                return vec![];
            }
            let subs = split_bash_subcommands(cmd);
            subs.into_iter()
                .filter_map(|sub| {
                    build_rule_text(tool, &serde_json::json!({"command": sub}), kind.clone())
                })
                .collect()
        }
        _ => build_rule_text(tool, input, kind)
            .map(|r| vec![r])
            .unwrap_or_default(),
    }
}

/// Bash 명령 문자열을 shell 연산자(`|`, `||`, `|&`, `&&`, `;`) 기준으로 sub-command로 분리한다.
///
/// # 동작
///
/// - quote(`"`, `'`) 내부의 연산자는 분리 기준에서 제외된다.
/// - `\` 로 이스케이프된 문자는 연산자 검출에서 제외된다.
/// - shlex 로 파싱 유효성 검증(닫히지 않은 quote 등)을 수행한다. 실패 시 `vec![cmd.trim()]` fallback.
/// - nested syntax (`$(...)`, `<(...)`, `<<EOF`, subshell `(...)`) 발견 시 분리 포기 → 단일 sub-command fallback
/// - `|&` (stderr pipe) 도 단일 연산자로 처리
/// - 각 sub-command의 leading/trailing whitespace는 제거된다.
/// - 빈 문자열 / whitespace-only → `vec![]`
///
/// # Examples
///
/// ```ignore
/// assert_eq!(split_bash_subcommands("echo hi | cat"), vec!["echo hi", "cat"]);
/// assert_eq!(split_bash_subcommands(r#"echo "a | b""#), vec![r#"echo "a | b""#]);
/// ```
pub fn split_bash_subcommands(cmd: &str) -> Vec<String> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return vec![];
    }

    // shlex 로 파싱 유효성 검증 (닫히지 않은 quote 등)
    // shlex::split() 이 None 을 반환하면 파싱 실패 → fallback
    if shlex::split(trimmed).is_none() {
        return vec![trimmed.to_string()];
    }

    // 보수적 fallback: nested shell syntax 발견 시 분리 포기 (review #298 w2)
    // - $(...) command substitution
    // - <(...) / >(...) process substitution
    // - <<EOF heredoc
    // - 단순 prefix `(` 시작은 subshell — 안전하게 단일 룰로 처리
    if trimmed.contains("$(")
        || trimmed.contains("<(")
        || trimmed.contains(">(")
        || trimmed.contains("<<")
        || trimmed.starts_with('(')
    {
        return vec![trimmed.to_string()];
    }

    // quote-aware state machine 으로 연산자 기준 분리
    split_by_shell_operators(trimmed)
}

/// quote/escape 를 인식하며 shell 연산자(`|`, `||`, `&&`, `;`) 기준으로 분리한다.
fn split_by_shell_operators(cmd: &str) -> Vec<String> {
    let chars: Vec<char> = cmd.chars().collect();
    let len = chars.len();

    let mut parts: Vec<String> = Vec::new();
    // 현재 sub-command 의 시작 인덱스
    let mut start = 0;
    let mut i = 0;

    // quote 상태: None / Some('"') / Some('\'')
    let mut in_quote: Option<char> = None;
    // 이전 문자가 backslash 이스케이프인가
    let mut escaped = false;

    while i < len {
        let ch = chars[i];

        if escaped {
            escaped = false;
            i += 1;
            continue;
        }

        if ch == '\\' && in_quote != Some('\'') {
            // single quote 안에서는 backslash 가 이스케이프 역할 안 함
            escaped = true;
            i += 1;
            continue;
        }

        match in_quote {
            Some(q) if ch == q => {
                // 닫는 quote
                in_quote = None;
            }
            Some(_) => {
                // quote 내부 — 그냥 통과
            }
            None => {
                if ch == '"' || ch == '\'' {
                    in_quote = Some(ch);
                } else if ch == '|' {
                    // `||`, `|&` (stderr pipe), 또는 단순 `|`
                    let op_end = if i + 1 < len && (chars[i + 1] == '|' || chars[i + 1] == '&') {
                        i + 2  // `||` 또는 `|&` (Bash stderr pipe)
                    } else {
                        i + 1
                    };
                    let segment = cmd[byte_index(cmd, start)..byte_index(cmd, i)].trim().to_string();
                    if !segment.is_empty() {
                        parts.push(segment);
                    }
                    start = op_end;
                    i = op_end;
                    continue;
                } else if ch == '&' && i + 1 < len && chars[i + 1] == '&' {
                    // `&&`
                    let segment = cmd[byte_index(cmd, start)..byte_index(cmd, i)].trim().to_string();
                    if !segment.is_empty() {
                        parts.push(segment);
                    }
                    start = i + 2;
                    i += 2;
                    continue;
                } else if ch == ';' {
                    let segment = cmd[byte_index(cmd, start)..byte_index(cmd, i)].trim().to_string();
                    if !segment.is_empty() {
                        parts.push(segment);
                    }
                    start = i + 1;
                }
            }
        }

        i += 1;
    }

    // 마지막 segment
    let tail = cmd[byte_index(cmd, start)..].trim().to_string();
    if !tail.is_empty() {
        parts.push(tail);
    }

    if parts.is_empty() {
        vec![cmd.trim().to_string()]
    } else {
        parts
    }
}

/// char 인덱스를 byte 인덱스로 변환한다.
fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
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

/// Claude Code Skill tool 의 input 에서 skill name 을 추출한다.
///
/// Claude Code Skill tool 의 input field 우선순위:
/// 1. `"name"` — 가장 일반적
/// 2. `"skill"` / `"skill_name"` — 보수적 fallback
fn skill_name(input: &serde_json::Value) -> Option<String> {
    ["name", "skill", "skill_name"]
        .iter()
        .filter_map(|key| input.get(key).and_then(|v| v.as_str()))
        .next()
        .map(|s| s.to_string())
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
    fn available_kinds_skill_with_name() {
        let kinds = available_rule_kinds("Skill", &json!({"name": "jira-new"}));
        assert_eq!(kinds, vec![RuleKind::Exact, RuleKind::Tool]);
    }

    #[test]
    fn available_kinds_skill_no_name() {
        let kinds = available_rule_kinds("Skill", &json!({}));
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
    fn build_skill_exact() {
        let result = build_rule_text("Skill", &json!({"name": "jira-new"}), RuleKind::Exact);
        assert_eq!(result, Some("Skill(jira-new)".to_string()));
    }

    #[test]
    fn build_skill_tool() {
        let result = build_rule_text("Skill", &json!({"name": "jira-new"}), RuleKind::Tool);
        assert_eq!(result, Some("Skill(*)".to_string()));
    }

    #[test]
    fn build_skill_exact_fallback_skill_field() {
        // "skill" field fallback
        let result = build_rule_text("Skill", &json!({"skill": "my-review"}), RuleKind::Exact);
        assert_eq!(result, Some("Skill(my-review)".to_string()));
    }

    #[test]
    fn build_skill_exact_no_name_returns_none() {
        // name field 없으면 None
        let result = build_rule_text("Skill", &json!({}), RuleKind::Exact);
        assert_eq!(result, None);
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

    // split_bash_subcommands 테스트

    #[test]
    fn split_empty() {
        assert_eq!(split_bash_subcommands(""), Vec::<String>::new());
    }

    #[test]
    fn split_whitespace_only() {
        assert_eq!(split_bash_subcommands("   "), Vec::<String>::new());
    }

    #[test]
    fn split_simple_no_pipe() {
        assert_eq!(
            split_bash_subcommands("echo hello"),
            vec!["echo hello".to_string()]
        );
    }

    #[test]
    fn split_pipe_two() {
        assert_eq!(
            split_bash_subcommands("cmd1 | cmd2"),
            vec!["cmd1".to_string(), "cmd2".to_string()]
        );
    }

    #[test]
    fn split_pipe_three() {
        assert_eq!(
            split_bash_subcommands("cmd1 | cmd2 | cmd3"),
            vec!["cmd1".to_string(), "cmd2".to_string(), "cmd3".to_string()]
        );
    }

    #[test]
    fn split_and_or_combo() {
        assert_eq!(
            split_bash_subcommands("cmd1 | cmd2 && cmd3"),
            vec!["cmd1".to_string(), "cmd2".to_string(), "cmd3".to_string()]
        );
    }

    #[test]
    fn split_semicolon() {
        assert_eq!(
            split_bash_subcommands("cmd1 ; cmd2"),
            vec!["cmd1".to_string(), "cmd2".to_string()]
        );
    }

    #[test]
    fn split_multiple_operators() {
        // `cmd1 | cmd2 && cmd3 ; cmd4` → 4개
        assert_eq!(
            split_bash_subcommands("cmd1 | cmd2 && cmd3 ; cmd4"),
            vec![
                "cmd1".to_string(),
                "cmd2".to_string(),
                "cmd3".to_string(),
                "cmd4".to_string()
            ]
        );
    }

    #[test]
    fn split_quoted_double() {
        // double quote 안의 `|` 는 보호됨
        assert_eq!(
            split_bash_subcommands(r#"echo "a | b""#),
            vec![r#"echo "a | b""#.to_string()]
        );
    }

    #[test]
    fn split_quoted_single() {
        // single quote 안의 `&&` 는 보호됨
        assert_eq!(
            split_bash_subcommands("echo 'a && b'"),
            vec!["echo 'a && b'".to_string()]
        );
    }

    #[test]
    fn split_escaped_pipe() {
        // escaped `|` 는 분리 기준이 아님
        assert_eq!(
            split_bash_subcommands(r"echo \| escape"),
            vec![r"echo \| escape".to_string()]
        );
    }

    #[test]
    fn split_trim_whitespace() {
        // 각 sub-command 의 leading/trailing whitespace 제거
        let result = split_bash_subcommands("  cmd1  |  cmd2  ");
        assert_eq!(result, vec!["cmd1".to_string(), "cmd2".to_string()]);
    }

    #[test]
    fn split_invalid_quote_fallback() {
        // 닫히지 않은 quote → fallback: 단일 sub-command
        let input = r#"echo "unclosed | pipe"#;
        assert_eq!(
            split_bash_subcommands(input),
            vec![input.to_string()]
        );
    }

    // review #298 w2: nested shell syntax fallback 테스트

    #[test]
    fn split_command_substitution_fallback() {
        // $(...) command substitution → 단일 sub-command fallback
        let input = "echo $(cmd1 | cmd2)";
        assert_eq!(
            split_bash_subcommands(input),
            vec![input.to_string()]
        );
    }

    #[test]
    fn split_subshell_fallback() {
        // (...) subshell prefix → 단일 sub-command fallback
        let input = "(cmd1 | cmd2)";
        assert_eq!(
            split_bash_subcommands(input),
            vec![input.to_string()]
        );
    }

    #[test]
    fn split_process_substitution_fallback() {
        // <(...) process substitution → 단일 sub-command fallback
        let input = "diff <(a | b) file";
        assert_eq!(
            split_bash_subcommands(input),
            vec![input.to_string()]
        );
    }

    #[test]
    fn split_heredoc_fallback() {
        // << heredoc → 단일 sub-command fallback
        let input = "cat <<EOF\nfoo | bar\nEOF";
        assert_eq!(
            split_bash_subcommands(input),
            vec![input.to_string()]
        );
    }

    // review #298 s1: `|&` (Bash stderr pipe) 테스트

    #[test]
    fn split_pipe_ampersand() {
        // `|&` 는 단일 연산자 → cmd1, cmd2 로 분리
        assert_eq!(
            split_bash_subcommands("cmd1 |& cmd2"),
            vec!["cmd1".to_string(), "cmd2".to_string()]
        );
    }

    #[test]
    fn split_pipe_ampersand_with_pipe() {
        // `|` 와 `|&` 혼합 → 3개
        assert_eq!(
            split_bash_subcommands("cmd1 | cmd2 |& cmd3"),
            vec!["cmd1".to_string(), "cmd2".to_string(), "cmd3".to_string()]
        );
    }

    // build_rule_texts 테스트

    #[test]
    fn build_texts_bash_exact_no_pipe() {
        let result = build_rule_texts("Bash", &json!({"command": "hostname"}), RuleKind::Exact);
        assert_eq!(result, vec!["Bash(hostname)".to_string()]);
    }

    #[test]
    fn build_texts_bash_exact_pipe() {
        let result = build_rule_texts(
            "Bash",
            &json!({"command": "find /tmp | head -3"}),
            RuleKind::Exact,
        );
        assert_eq!(
            result,
            vec!["Bash(find /tmp)".to_string(), "Bash(head -3)".to_string()]
        );
    }

    #[test]
    fn build_texts_bash_prefix_pipe() {
        let result = build_rule_texts(
            "Bash",
            &json!({"command": "find /tmp | head -3"}),
            RuleKind::Prefix,
        );
        assert_eq!(
            result,
            vec!["Bash(find *)".to_string(), "Bash(head *)".to_string()]
        );
    }

    #[test]
    fn build_texts_bash_tool_pipe() {
        let result = build_rule_texts(
            "Bash",
            &json!({"command": "find /tmp | head -3"}),
            RuleKind::Tool,
        );
        assert_eq!(result, vec!["Bash(*)".to_string()]);
    }

    #[test]
    fn build_texts_webfetch_domain() {
        let result = build_rule_texts(
            "WebFetch",
            &json!({"url": "https://api.example.com/v1"}),
            RuleKind::Domain,
        );
        assert_eq!(result, vec!["WebFetch(domain:api.example.com)".to_string()]);
    }

    #[test]
    fn build_texts_invalid_returns_empty() {
        // Bash + Exact, command 없음 → vec![]
        let result = build_rule_texts("Bash", &json!({}), RuleKind::Exact);
        assert_eq!(result, Vec::<String>::new());
    }

    // MCP tool 테스트 (#308)

    #[test]
    fn build_rule_text_mcp_tool_returns_exact_no_parens() {
        // MCP tool 은 괄호 없는 exact form (invalid `mcp__server__tool(*)` 방지)
        let result = build_rule_text("mcp__server__tool", &json!({}), RuleKind::Tool);
        assert_eq!(result, Some("mcp__server__tool".to_string()));
    }

    #[test]
    fn build_rule_text_native_tool_keeps_parens() {
        // native tool 은 기존 동작 유지 — regression guard
        let result = build_rule_text("Bash", &json!({}), RuleKind::Tool);
        assert_eq!(result, Some("Bash(*)".to_string()));
    }

    #[test]
    fn available_rule_kinds_mcp_returns_tool_only() {
        // MCP tool 은 Tool 만 (의도 명시, unknown fallback 과 동작 동일)
        let kinds = available_rule_kinds("mcp__server__tool", &json!({}));
        assert_eq!(kinds, vec![RuleKind::Tool]);
    }
}
