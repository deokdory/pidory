#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use poise::serenity_prelude::UserId;

use crate::claude_settings::rule::{RuleKind, Scope};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    AllowAlways { rule_kind: RuleKind, scope: Scope },
    Deny,
    Answer(HashMap<String, String>),
}

pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub tool_use_id: String,
    pub input: serde_json::Value,
    pub decision_reason: Option<String>,
    pub response_tx: tokio::sync::oneshot::Sender<PermissionDecision>,
    /// Receives `()` when the worker has auto-denied this request due to a 5-minute timeout.
    /// The permission handler awaits this to disable buttons and append the timeout label.
    pub timeout_rx: tokio::sync::oneshot::Receiver<()>,
    pub triggered_by: UserId,
    /// Worker session's project directory (used by path_safety check)
    pub cwd: PathBuf,
    /// Resolved settings additionalDirectories (Arc shared across requests in same session)
    pub additional_dirs: Arc<Vec<PathBuf>>,
}

pub struct PermissionCache {
    allowed_tools: HashSet<String>,
    allowed_rules: Vec<String>,
}

impl PermissionCache {
    pub fn new() -> Self {
        Self {
            allowed_tools: HashSet::new(),
            allowed_rules: Vec::new(),
        }
    }

    pub fn is_always_allowed(&self, tool_name: &str) -> bool {
        self.allowed_tools.contains(tool_name)
    }

    pub fn matches(&self, tool: &str, input: &serde_json::Value) -> bool {
        if self.is_always_allowed(tool) {
            return true;
        }
        for rule_str in &self.allowed_rules {
            if crate::claude_settings::rule::rule_matches(rule_str, tool, input) {
                return true;
            }
        }
        false
    }

    pub fn add_always_allow(&mut self, tool_name: &str) {
        self.allowed_tools.insert(tool_name.to_string());
    }

    /// rule_str 을 추가한다. 중복이면 추가하지 않는다.
    /// 반환: 새로 삽입되면 `true`, 중복이면 `false`.
    pub fn add_rule(&mut self, rule_str: String) -> bool {
        if self.allowed_rules.iter().any(|r| r == &rule_str) {
            return false;
        }
        self.allowed_rules.push(rule_str);
        true
    }

    pub fn clear(&mut self) {
        self.allowed_tools.clear();
        self.allowed_rules.clear();
    }

    pub fn clear_tool(&mut self, tool_name: &str) {
        self.allowed_tools.remove(tool_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_empty_by_default() {
        let cache = PermissionCache::new();
        assert!(!cache.is_always_allowed("Bash"));
        assert!(!cache.is_always_allowed("Write"));
    }

    #[test]
    fn cache_add_and_check() {
        let mut cache = PermissionCache::new();
        cache.add_always_allow("Bash");
        assert!(cache.is_always_allowed("Bash"));
        assert!(!cache.is_always_allowed("Write"));
    }

    #[test]
    fn cache_add_multiple() {
        let mut cache = PermissionCache::new();
        cache.add_always_allow("Bash");
        cache.add_always_allow("Write");
        assert!(cache.is_always_allowed("Bash"));
        assert!(cache.is_always_allowed("Write"));
        assert!(!cache.is_always_allowed("Edit"));
    }

    #[test]
    fn cache_clear() {
        let mut cache = PermissionCache::new();
        cache.add_always_allow("Bash");
        cache.add_always_allow("Write");
        cache.clear();
        assert!(!cache.is_always_allowed("Bash"));
        assert!(!cache.is_always_allowed("Write"));
    }

    #[test]
    fn cache_add_idempotent() {
        let mut cache = PermissionCache::new();
        cache.add_always_allow("Bash");
        cache.add_always_allow("Bash");
        assert!(cache.is_always_allowed("Bash"));
    }

    #[test]
    fn decision_enum_values() {
        let allow = PermissionDecision::Allow;
        let always = PermissionDecision::AllowAlways {
            rule_kind: RuleKind::Exact,
            scope: Scope::Project,
        };
        let deny = PermissionDecision::Deny;
        assert_ne!(allow, always);
        assert_ne!(allow, deny);
        assert_ne!(always, deny);
    }

    #[test]
    fn cache_clear_tool_removes_single() {
        let mut cache = PermissionCache::new();
        cache.add_always_allow("Bash");
        cache.add_always_allow("Write");
        cache.clear_tool("Bash");
        assert!(!cache.is_always_allowed("Bash"));
        assert!(cache.is_always_allowed("Write"));
    }

    #[test]
    fn allow_always_destructures() {
        let decision = PermissionDecision::AllowAlways {
            rule_kind: RuleKind::Prefix,
            scope: Scope::Global,
        };
        match decision {
            PermissionDecision::AllowAlways { rule_kind, scope } => {
                assert_eq!(rule_kind, RuleKind::Prefix);
                assert_eq!(scope, Scope::Global);
            }
            _ => panic!("expected AllowAlways"),
        }
    }

    // --- rule kind 별 matches 테스트 ---

    #[test]
    fn matches_exact_rule_hit_and_miss() {
        // Exact rule: Edit(/tmp/foo.rs) → file_path 정확 일치만 매칭
        let mut cache = PermissionCache::new();
        cache.add_rule("Edit(/tmp/foo.rs)".to_string());

        assert!(cache.matches("Edit", &serde_json::json!({"file_path": "/tmp/foo.rs"})));
        assert!(!cache.matches("Edit", &serde_json::json!({"file_path": "/tmp/bar.rs"})));
    }

    #[test]
    fn matches_prefix_rule_bash() {
        // Prefix rule: Bash(npm *) → "npm test", "npm install" 매칭 / "npx test", "npmtest" 불일치
        let mut cache = PermissionCache::new();
        cache.add_rule("Bash(npm *)".to_string());

        assert!(cache.matches("Bash", &serde_json::json!({"command": "npm test"})));
        assert!(cache.matches("Bash", &serde_json::json!({"command": "npm install"})));
        assert!(!cache.matches("Bash", &serde_json::json!({"command": "npx test"})));
        assert!(!cache.matches("Bash", &serde_json::json!({"command": "npmtest"})));
    }

    #[test]
    fn matches_domain_rule_webfetch() {
        // Domain rule: WebFetch(domain:example.com) → example.com 매칭, evil.com 불일치
        let mut cache = PermissionCache::new();
        cache.add_rule("WebFetch(domain:example.com)".to_string());

        assert!(cache.matches("WebFetch", &serde_json::json!({"url": "https://example.com/page"})));
        assert!(!cache.matches("WebFetch", &serde_json::json!({"url": "https://evil.com/page"})));
    }

    #[test]
    fn matches_path_namespace_isolation() {
        // Path namespace 격리: Read(/x) 추가 시 Write 호출에 매칭 X, Read 호출에 매칭 O
        let mut cache = PermissionCache::new();
        cache.add_rule("Read(/x)".to_string());

        assert!(cache.matches("Read", &serde_json::json!({"file_path": "/x"})));
        assert!(!cache.matches("Write", &serde_json::json!({"file_path": "/x"})));
        assert!(!cache.matches("Edit", &serde_json::json!({"file_path": "/x"})));
    }

    #[test]
    fn add_rule_dedup_prevents_duplicate_push() {
        let mut cache = PermissionCache::new();
        assert!(cache.add_rule("Bash(*)".to_string()), "first add returns true");
        assert!(!cache.add_rule("Bash(*)".to_string()), "duplicate add returns false");
        // 실제로 하나만 저장되었는지 matches 로 확인
        assert!(cache.matches("Bash", &serde_json::json!({"command": "anything"})));
    }

    #[test]
    fn matches_mcp_tool_bare_form() {
        // MCP tool: bare form (괄호 없음) 매칭 O, 괄호 form 불일치
        let mut cache = PermissionCache::new();
        cache.add_rule("mcp__pidory__skill".to_string());

        assert!(cache.matches("mcp__pidory__skill", &serde_json::json!({})));
        // 다른 MCP tool 은 매칭 X
        assert!(!cache.matches("mcp__other__tool", &serde_json::json!({})));
    }

    #[test]
    fn matches_mcp_tool_parenthesized_form_invalid() {
        // MCP tool: 괄호 form rule 은 invalid — 매칭 X
        let mut cache = PermissionCache::new();
        cache.add_rule("mcp__pidory__skill(*)".to_string());

        assert!(!cache.matches("mcp__pidory__skill", &serde_json::json!({})));
    }
}
