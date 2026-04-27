use std::collections::HashSet;
use std::time::Instant;
use poise::serenity_prelude::{MessageId, UserId};

/// Per-thread session state, keyed by Discord thread_id in `Data.session_states`.
///
/// # Lock ordering
/// When holding both `Data.sessions` and `Data.session_states` locks,
/// always acquire `sessions` first, then `session_states`. Reverse order
/// risks deadlock.
#[derive(Default)]
pub struct SessionState {
    pub skills: Vec<String>,
    pub needs_context: bool,
    pub archived: bool,
    pub turn_initiator: Option<UserId>,
    pub turn_participants: HashSet<UserId>,
    pub last_tool_name: Option<String>,
    pub kick_cooldown: Option<Instant>,
    pub kick_pending: bool,
    pub next_step_button: Option<MessageId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_is_empty() {
        let s = SessionState::default();
        assert!(s.skills.is_empty());
        assert!(!s.needs_context);
        assert!(!s.archived);
        assert!(s.turn_initiator.is_none());
        assert!(s.turn_participants.is_empty());
        assert!(s.last_tool_name.is_none());
        assert!(s.kick_cooldown.is_none());
        assert!(!s.kick_pending);
        assert!(s.next_step_button.is_none());
    }
}
