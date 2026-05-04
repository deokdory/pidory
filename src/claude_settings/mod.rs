//! Claude `settings.json` / `settings.local.json` atomic editor.
//!
//! 7-layer 동시성 보호 + dedup. NP12 Plan C 핵심 인프라.
//!
//! # Quick start
//!
//! ```no_run
//! # use pidory::claude_settings::{add_permission, LoggingNotifier};
//! # use std::path::Path;
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! add_permission(
//!     Path::new("/tmp/settings.json"),
//!     "Bash(npm *)",
//!     &LoggingNotifier,
//! ).await?;
//! # Ok(()) }
//! ```
//!
//! # Module structure
//!
//! - [`add_permission`] — public API, atomic permission rule insertion
//! - [`ConflictNotifier`] — trait for callback on conflicts (P1.2 implementor)
//! - [`LoggingNotifier`] — default `tracing::warn!`-based notifier
//! - [`cleanup_leftover_temp`] — startup helper (caller: P1.5)

mod error;
mod path;
mod integrity;
mod dedup;
mod lock;
mod cleanup;
mod notifier;
mod editor;
pub mod rule;
pub mod danger;

#[allow(unused_imports)]
pub use error::ClaudeSettingsError;
#[allow(unused_imports)]
pub use editor::add_permission;
#[allow(unused_imports)]
pub use notifier::{ConflictEvent, ConflictNotifier, LoggingNotifier, MergeOutcome};
#[allow(unused_imports)]
pub use cleanup::cleanup_leftover_temp;
// RMW core (apply_mutation)는 pub(crate)만 — P1.4 진입 시 승격 (Plan Must NOT Have)
#[allow(unused_imports)]
pub use rule::{RuleKind, Scope, scope_to_path, available_rule_kinds, build_rule_text};
#[allow(unused_imports)]
pub use danger::{Severity, classify_command};
