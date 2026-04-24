use crate::handler::reset_ui::ResetAction;

/// Discriminated union of every button/select custom_id format the bot produces.
///
/// `from_custom_id` is a pure function: no I/O, no side effects, no allocations
/// beyond the returned `String` fields.
///
/// Prefix matching order (CRITICAL — must not be changed):
/// 1. `perm:`
/// 2. `ask_cancel_confirm:` (before `ask_cancel:`)
/// 3. `ask_cancel_abort:`   (before `ask_cancel:`)
/// 4. `ask_cancel:`
/// 5. `ask_text:`
/// 6. `ask_sel:`
/// 7. `ask:`               (after all other `ask_*` prefixes)
/// 8. `reset:`
/// 9. `nxt:`
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InteractionKind {
    Permission {
        request_id: String,
        action: PermissionAction,
    },
    QuestionOption {
        request_id: String,
        index: usize,
    },
    QuestionText {
        request_id: String,
    },
    QuestionSelect {
        request_id: String,
    },
    QuestionCancel {
        request_id: String,
        stage: CancelStage,
    },
    Reset {
        thread_id: String,
        action: ResetAction,
    },
    NextStep {
        thread_id: String,
        skill: String,
    },
}

/// Button action for permission prompts (`perm:<rid>:<action>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionAction {
    Allow,
    Always,
    Deny,
}

/// Which stage of the cancel confirmation flow a button belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancelStage {
    /// `ask_cancel:<rid>` — initial cancel button
    Ask,
    /// `ask_cancel_confirm:<rid>` — "yes, cancel" confirmation
    Confirm,
    /// `ask_cancel_abort:<rid>` — "no, keep going" (abort the cancel)
    Abort,
}

impl InteractionKind {
    /// Parses a Discord component `custom_id` string into an `InteractionKind`.
    ///
    /// Returns `None` if the string does not match any known format or contains
    /// invalid field values (e.g. non-numeric index, unknown action token).
    pub(crate) fn from_custom_id(s: &str) -> Option<Self> {
        // Order is load-bearing — see module-level comment.
        if let Some(rest) = s.strip_prefix("perm:") {
            let (request_id, action_str) = rest.rsplit_once(':')?;
            if request_id.is_empty() {
                return None;
            }
            let action = match action_str {
                "allow" => PermissionAction::Allow,
                "always" => PermissionAction::Always,
                "deny" => PermissionAction::Deny,
                _ => return None,
            };
            Some(Self::Permission {
                request_id: request_id.to_string(),
                action,
            })
        } else if let Some(rest) = s.strip_prefix("ask_cancel_confirm:") {
            Some(Self::QuestionCancel {
                request_id: rest.to_string(),
                stage: CancelStage::Confirm,
            })
        } else if let Some(rest) = s.strip_prefix("ask_cancel_abort:") {
            Some(Self::QuestionCancel {
                request_id: rest.to_string(),
                stage: CancelStage::Abort,
            })
        } else if let Some(rest) = s.strip_prefix("ask_cancel:") {
            Some(Self::QuestionCancel {
                request_id: rest.to_string(),
                stage: CancelStage::Ask,
            })
        } else if let Some(rest) = s.strip_prefix("ask_text:") {
            Some(Self::QuestionText {
                request_id: rest.to_string(),
            })
        } else if let Some(rest) = s.strip_prefix("ask_sel:") {
            Some(Self::QuestionSelect {
                request_id: rest.to_string(),
            })
        } else if let Some(rest) = s.strip_prefix("ask:") {
            let (request_id, index_str) = rest.rsplit_once(':')?;
            let index: usize = index_str.parse().ok()?;
            Some(Self::QuestionOption {
                request_id: request_id.to_string(),
                index,
            })
        } else if let Some(rest) = s.strip_prefix("reset:") {
            let (thread_id, action_str) = rest.rsplit_once(':')?;
            if thread_id.is_empty() {
                return None;
            }
            let action = match action_str {
                "confirm" => ResetAction::Confirm,
                "cancel" => ResetAction::Cancel,
                _ => return None,
            };
            Some(Self::Reset {
                thread_id: thread_id.to_string(),
                action,
            })
        } else if let Some(rest) = s.strip_prefix("nxt:") {
            // thread_id is a Discord snowflake (no colons); skill is the last segment.
            // rsplit_once is correct here: skill cannot contain colons by convention,
            // and thread_id is always a numeric snowflake.
            let (thread_id, skill) = rest.rsplit_once(':')?;
            if thread_id.is_empty() || skill.is_empty() {
                return None;
            }
            Some(Self::NextStep {
                thread_id: thread_id.to_string(),
                skill: skill.to_string(),
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::reset_ui::ResetAction;

    // ── Permission ──────────────────────────────────────────────────────────

    #[test]
    fn parse_permission_allow() {
        let result = InteractionKind::from_custom_id("perm:abc:allow");
        assert_eq!(
            result,
            Some(InteractionKind::Permission {
                request_id: "abc".to_string(),
                action: PermissionAction::Allow,
            })
        );
    }

    #[test]
    fn parse_permission_always() {
        let result = InteractionKind::from_custom_id("perm:abc:always");
        assert_eq!(
            result,
            Some(InteractionKind::Permission {
                request_id: "abc".to_string(),
                action: PermissionAction::Always,
            })
        );
    }

    #[test]
    fn parse_permission_deny() {
        let result = InteractionKind::from_custom_id("perm:abc:deny");
        assert_eq!(
            result,
            Some(InteractionKind::Permission {
                request_id: "abc".to_string(),
                action: PermissionAction::Deny,
            })
        );
    }

    // ── QuestionOption ───────────────────────────────────────────────────────

    #[test]
    fn parse_question_option() {
        let result = InteractionKind::from_custom_id("ask:rid:0");
        assert_eq!(
            result,
            Some(InteractionKind::QuestionOption {
                request_id: "rid".to_string(),
                index: 0,
            })
        );
    }

    // ── QuestionText ─────────────────────────────────────────────────────────

    #[test]
    fn parse_question_text() {
        let result = InteractionKind::from_custom_id("ask_text:rid");
        assert_eq!(
            result,
            Some(InteractionKind::QuestionText {
                request_id: "rid".to_string(),
            })
        );
    }

    // ── QuestionSelect ───────────────────────────────────────────────────────

    #[test]
    fn parse_question_select() {
        let result = InteractionKind::from_custom_id("ask_sel:rid");
        assert_eq!(
            result,
            Some(InteractionKind::QuestionSelect {
                request_id: "rid".to_string(),
            })
        );
    }

    // ── Reset ─────────────────────────────────────────────────────────────────

    #[test]
    fn parse_reset_confirm() {
        let result = InteractionKind::from_custom_id("reset:tid:confirm");
        assert_eq!(
            result,
            Some(InteractionKind::Reset {
                thread_id: "tid".to_string(),
                action: ResetAction::Confirm,
            })
        );
    }

    #[test]
    fn parse_reset_cancel() {
        let result = InteractionKind::from_custom_id("reset:tid:cancel");
        assert_eq!(
            result,
            Some(InteractionKind::Reset {
                thread_id: "tid".to_string(),
                action: ResetAction::Cancel,
            })
        );
    }

    // ── NextStep ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_next_step() {
        let result = InteractionKind::from_custom_id("nxt:tid:skill_name");
        assert_eq!(
            result,
            Some(InteractionKind::NextStep {
                thread_id: "tid".to_string(),
                skill: "skill_name".to_string(),
            })
        );
    }

    // ── Prefix collision regression ───────────────────────────────────────────

    #[test]
    fn parse_ask_cancel_returns_ask() {
        let result = InteractionKind::from_custom_id("ask_cancel:foo");
        assert_eq!(
            result,
            Some(InteractionKind::QuestionCancel {
                request_id: "foo".to_string(),
                stage: CancelStage::Ask,
            })
        );
    }

    #[test]
    fn parse_ask_cancel_confirm_returns_confirm() {
        let result = InteractionKind::from_custom_id("ask_cancel_confirm:foo");
        assert_eq!(
            result,
            Some(InteractionKind::QuestionCancel {
                request_id: "foo".to_string(),
                stage: CancelStage::Confirm,
            })
        );
    }

    #[test]
    fn parse_ask_cancel_abort_returns_abort() {
        let result = InteractionKind::from_custom_id("ask_cancel_abort:foo");
        assert_eq!(
            result,
            Some(InteractionKind::QuestionCancel {
                request_id: "foo".to_string(),
                stage: CancelStage::Abort,
            })
        );
    }

    // ── Negative cases ────────────────────────────────────────────────────────

    #[test]
    fn empty_returns_none() {
        assert_eq!(InteractionKind::from_custom_id(""), None);
    }

    #[test]
    fn incomplete_perm_returns_none() {
        assert_eq!(InteractionKind::from_custom_id("perm:"), None);
    }

    #[test]
    fn invalid_action_returns_none() {
        assert_eq!(
            InteractionKind::from_custom_id("perm:xxx:invalid_action"),
            None
        );
    }

    #[test]
    fn non_numeric_index_returns_none() {
        assert_eq!(
            InteractionKind::from_custom_id("ask:rid:not_a_number"),
            None
        );
    }

    #[test]
    fn unknown_prefix_returns_none() {
        assert_eq!(InteractionKind::from_custom_id("unknown_prefix:foo"), None);
    }

    #[test]
    fn invalid_reset_action_returns_none() {
        assert_eq!(InteractionKind::from_custom_id("reset:tid:bogus"), None);
    }

    #[test]
    fn empty_thread_id_reset_returns_none() {
        assert_eq!(InteractionKind::from_custom_id("reset::confirm"), None);
    }

    #[test]
    fn empty_thread_id_nxt_returns_none() {
        assert_eq!(InteractionKind::from_custom_id("nxt::skill"), None);
    }
}
