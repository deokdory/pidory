#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use poise::serenity_prelude::UserId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    AlwaysAllow,
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
    pub triggered_by: UserId,
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
        let always = PermissionDecision::AlwaysAllow;
        let deny = PermissionDecision::Deny;
        assert_ne!(allow, always);
        assert_ne!(allow, deny);
        assert_ne!(always, deny);
    }
}
