#![allow(dead_code)]

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    AlwaysAllow,
    Deny,
}

pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub tool_use_id: String,
    pub input: serde_json::Value,
    pub decision_reason: Option<String>,
    pub response_tx: tokio::sync::oneshot::Sender<PermissionDecision>,
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


use poise::serenity_prelude::{ChannelId, Context};
use tokio::sync::mpsc;
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::Mutex;
use tracing::warn;

pub async fn run_permission_handler(
    mut permission_rx: mpsc::Receiver<PermissionRequest>,
    ctx: Context,
    channel_id: ChannelId,
    pending_permissions: Arc<Mutex<HashMap<String, crate::PendingPermission>>>,
    owner_id: u64,
    lang: crate::i18n::Lang,
    thread_id: String,
) {
    while let Some(perm_req) = permission_rx.recv().await {
        let msg = crate::handler::permission_ui::create_permission_message(
            &perm_req.tool_name,
            &perm_req.input,
            &perm_req.request_id,
            perm_req.decision_reason.as_deref(),
            owner_id,
            lang,
        );

        match channel_id.send_message(&ctx, msg).await {
            Ok(sent) => {
                let pending = crate::PendingPermission {
                    response_tx: perm_req.response_tx,
                    tool_name: perm_req.tool_name,
                    message_id: sent.id,
                    thread_id: thread_id.clone(),
                };
                pending_permissions
                    .lock()
                    .await
                    .insert(perm_req.request_id, pending);
            }
            Err(e) => {
                warn!("Failed to send permission message: {}", e);
                let _ = perm_req.response_tx.send(PermissionDecision::Deny);
            }
        }
    }
    tracing::info!("Permission handler task exiting (sender dropped)");
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
