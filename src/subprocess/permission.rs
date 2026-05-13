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
}

impl PermissionCache {
    pub fn new() -> Self {
        Self {
            allowed_tools: HashSet::new(),
        }
    }

    pub fn is_always_allowed(&self, tool_name: &str) -> bool {
        self.allowed_tools.contains(tool_name)
    }

    pub fn add_always_allow(&mut self, tool_name: &str) {
        self.allowed_tools.insert(tool_name.to_string());
    }

    pub fn clear(&mut self) {
        self.allowed_tools.clear();
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
}
