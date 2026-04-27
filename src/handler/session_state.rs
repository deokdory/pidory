use std::collections::HashSet;
use std::time::Instant;
use poise::serenity_prelude::{ChannelId, MessageId, UserId};
use crate::handler::todo_tracker::TodoTracker;

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
    pub todo_tracker: Option<TodoTracker>,
}

impl SessionState {
    /// take/put 패턴 운용 규칙:
    ///
    /// 1. session_states 락을 짧게 잡고 즉시 `take_todo_tracker`로 ownership을 꺼냄.
    /// 2. 락을 drop한 뒤 락 밖에서 tracker.update/flush/cleanup (Discord HTTP) 호출.
    /// 3. 락을 다시 짧게 잡고 `archived` 상태 확인:
    ///    - `archived=false`면 `put_todo_tracker`로 돌려놓기.
    ///    - `archived=true`면 락 풀고 `tracker.cleanup(ctx)` 호출 후 drop (tombstone 의미 보존).
    ///
    /// panic safety: take 후 panic 발생 시 tracker 손실. 호출 사이트는
    /// take/put 페어를 같은 panic-free async fn 안에서 끝내야 함.
    ///
    /// `channel_id`는 lazy-init용 — `todo_tracker`가 None이면 `TodoTracker::new(channel_id)`로 새로 생성.
    ///
    /// **락 보유 중 await 호출 절대 금지.**
    pub fn take_todo_tracker(&mut self, channel_id: ChannelId) -> TodoTracker {
        self.todo_tracker
            .take()
            .unwrap_or_else(|| TodoTracker::new(channel_id))
    }

    /// take한 TodoTracker를 되돌려놓는다. take/put 패턴의 put 절반.
    ///
    /// archived=true인 경우 put을 호출하지 말고 caller 측에서 cleanup 후 drop할 것.
    pub fn put_todo_tracker(&mut self, tracker: TodoTracker) {
        self.todo_tracker = Some(tracker);
    }
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

    #[test]
    fn default_todo_tracker_is_none() {
        let s = SessionState::default();
        assert!(s.todo_tracker.is_none());
    }

    #[test]
    fn take_creates_when_absent_and_does_not_persist() {
        use poise::serenity_prelude::ChannelId;
        let mut s = SessionState::default();
        assert!(s.todo_tracker.is_none());
        let _tracker = s.take_todo_tracker(ChannelId::new(1));
        // take 후 필드는 비어있어야 함 (take 의미: ownership 이동)
        assert!(s.todo_tracker.is_none());
    }

    #[test]
    fn take_put_roundtrip_makes_field_some() {
        use poise::serenity_prelude::ChannelId;
        let mut s = SessionState::default();
        let tracker = s.take_todo_tracker(ChannelId::new(42));
        assert!(s.todo_tracker.is_none());
        s.put_todo_tracker(tracker);
        assert!(s.todo_tracker.is_some());
        // 두 번째 take도 정상 동작 (이번엔 기존 인스턴스 반환)
        let _tracker2 = s.take_todo_tracker(ChannelId::new(42));
        assert!(s.todo_tracker.is_none());
    }
}
