use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use poise::serenity_prelude::{ChannelId, MessageId, UserId};
use tokio::sync::Mutex;
use crate::handler::todo_tracker::TodoTracker;

/// SessionState 내 TodoTracker의 라이프사이클을 명시 모델링한다.
///
/// `Option<TodoTracker>`만으로는 'None'이 '미초기화'와 'checked out'을 구분 못해
/// race window에서 두 번째 take가 새 인스턴스를 만들어 dual tracker가 발생할 수 있음.
/// `CheckedOut` variant로 점유 상태를 명시 표현한다.
#[derive(Default)]
pub enum TodoTrackerSlot {
    /// 한 번도 초기화되지 않음. 다음 take가 lazy-init한다.
    #[default]
    None,
    /// tracker가 슬롯에 보관 중. 다음 take가 ownership을 받는다.
    Present(TodoTracker),
    /// 다른 async path가 take해서 Discord HTTP 중. 두 번째 take는 silent skip.
    CheckedOut,
}

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
    pub todo_tracker_slot: TodoTrackerSlot,
}

impl SessionState {
    /// CheckedOut으로 전환하고 tracker ownership을 반환한다.
    /// - `None` → `CheckedOut`으로 전환, 새 `TodoTracker::new(channel_id)` 반환 (lazy-init)
    /// - `Present(t)` → `CheckedOut`으로 전환, 기존 t 반환
    /// - `CheckedOut` → 그대로 두고 None 반환 (두 번째 take는 silent skip)
    ///
    /// 호출 사이트는 None을 받으면 즉시 return해라 (또는 caller-specific skip 로직).
    pub fn try_take_todo_tracker(&mut self, channel_id: ChannelId) -> Option<TodoTracker> {
        match std::mem::replace(&mut self.todo_tracker_slot, TodoTrackerSlot::CheckedOut) {
            TodoTrackerSlot::None => Some(TodoTracker::new(channel_id)),
            TodoTrackerSlot::Present(t) => Some(t),
            TodoTrackerSlot::CheckedOut => {
                // 다른 path가 take 중. CheckedOut 유지하고 None 반환.
                self.todo_tracker_slot = TodoTrackerSlot::CheckedOut;
                None
            }
        }
    }

    /// take한 tracker를 슬롯에 다시 꽂는다. CheckedOut → Present(t) 전환.
    /// CheckedOut가 아닐 때 호출하면 이전 상태를 덮어쓰지만 그건 호출 사이트 버그다.
    pub fn put_todo_tracker(&mut self, tracker: TodoTracker) {
        self.todo_tracker_slot = TodoTrackerSlot::Present(tracker);
    }

    /// archived 시 cleanup 후 슬롯을 None으로 되돌린다.
    pub fn drop_todo_tracker(&mut self) {
        self.todo_tracker_slot = TodoTrackerSlot::None;
    }

    /// 슬롯에 보관 중인 tracker를 추출(Present만). cleanup 경로에서 사용.
    /// CheckedOut인 경우(다른 path가 들고 있음) None을 반환하고 슬롯은 CheckedOut 유지.
    /// 이러면 take 중인 tracker는 own한 path가 cleanup 책임.
    pub fn take_present_todo_tracker(&mut self) -> Option<TodoTracker> {
        match &self.todo_tracker_slot {
            TodoTrackerSlot::Present(_) => {
                if let TodoTrackerSlot::Present(t) = std::mem::replace(&mut self.todo_tracker_slot, TodoTrackerSlot::None) {
                    Some(t)
                } else {
                    unreachable!()
                }
            }
            _ => None,
        }
    }
}

/// 살아있는 세션이 있을 때만 todo_tracker를 take. 세션이 없으면 None을 반환해
/// caller가 부활(entry().or_default())을 피하게 한다. 두 번째 take(CheckedOut)도 None 반환.
///
/// w1 (cleanup된 세션 부활 방지) + c1 (CheckedOut silent skip) 동시 해결.
pub async fn try_acquire_todo_tracker(
    session_states: &Arc<Mutex<HashMap<String, SessionState>>>,
    thread_id: &str,
    channel_id: ChannelId,
) -> Option<TodoTracker> {
    let mut guard = session_states.lock().await;
    guard
        .get_mut(thread_id)
        .and_then(|st| st.try_take_todo_tracker(channel_id))
}

/// take한 tracker를 락 밖에서 정리한다. 호출 사이트에서 update/flush를 마친 직후 호출.
///
/// - 세션이 살아있고 !archived → put (CheckedOut → Present)
/// - 세션이 archived → drop_todo_tracker로 슬롯 비우고 락 밖에서 cleanup
/// - 세션이 사라짐 → 그냥 락 밖에서 cleanup (CheckedOut 흔적 없음)
///
/// w3 (5중복 helper 통합) + w1 (부활 금지) 동시 해결.
pub async fn release_todo_tracker(
    session_states: &Arc<Mutex<HashMap<String, SessionState>>>,
    thread_id: &str,
    tracker: TodoTracker,
    ctx: &poise::serenity_prelude::Context,
) {
    let to_cleanup = {
        let mut guard = session_states.lock().await;
        match guard.get_mut(thread_id) {
            Some(st) if !st.archived => {
                st.put_todo_tracker(tracker);
                None
            }
            Some(st) => {
                // archived: 슬롯을 None으로 되돌리고 cleanup은 락 밖에서
                st.drop_todo_tracker();
                Some(tracker)
            }
            None => Some(tracker),
        }
    };
    if let Some(mut t) = to_cleanup {
        t.cleanup(ctx).await;
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
    fn default_todo_tracker_slot_is_none() {
        let s = SessionState::default();
        assert!(matches!(s.todo_tracker_slot, TodoTrackerSlot::None));
    }

    #[test]
    fn try_take_returns_none_when_checked_out() {
        let mut s = SessionState::default();
        let _t1 = s.try_take_todo_tracker(ChannelId::new(1)).expect("first take ok");
        // 두 번째 take는 None (silent skip)
        assert!(s.try_take_todo_tracker(ChannelId::new(1)).is_none());
    }

    #[test]
    fn put_after_take_restores_present() {
        let mut s = SessionState::default();
        let t = s.try_take_todo_tracker(ChannelId::new(1)).expect("take");
        s.put_todo_tracker(t);
        // 다시 take 가능
        let _t2 = s.try_take_todo_tracker(ChannelId::new(1)).expect("re-take");
    }

    #[test]
    fn drop_after_take_resets_to_none() {
        let mut s = SessionState::default();
        let _t = s.try_take_todo_tracker(ChannelId::new(1)).expect("take");
        s.drop_todo_tracker();
        // None 상태로 돌아왔으니 take 가능 (lazy-init)
        let _t2 = s.try_take_todo_tracker(ChannelId::new(1)).expect("re-take after drop");
    }

    #[test]
    fn take_present_only_returns_present_variant() {
        let mut s = SessionState::default();
        // None 상태: take_present_todo_tracker는 None 반환
        assert!(s.take_present_todo_tracker().is_none());
        // CheckedOut 상태: 여전히 None
        let t = s.try_take_todo_tracker(ChannelId::new(1)).expect("take");
        assert!(s.take_present_todo_tracker().is_none());
        s.put_todo_tracker(t);
        // Present 상태: Some 반환
        assert!(s.take_present_todo_tracker().is_some());
    }

    #[test]
    fn take_creates_when_absent_and_slot_becomes_checked_out() {
        let mut s = SessionState::default();
        assert!(matches!(s.todo_tracker_slot, TodoTrackerSlot::None));
        let _tracker = s.try_take_todo_tracker(ChannelId::new(1));
        assert!(_tracker.is_some());
        // take 후 슬롯은 CheckedOut
        assert!(matches!(s.todo_tracker_slot, TodoTrackerSlot::CheckedOut));
    }

    #[test]
    fn take_put_roundtrip_makes_slot_present() {
        let mut s = SessionState::default();
        let tracker = s.try_take_todo_tracker(ChannelId::new(42)).expect("take");
        assert!(matches!(s.todo_tracker_slot, TodoTrackerSlot::CheckedOut));
        s.put_todo_tracker(tracker);
        assert!(matches!(s.todo_tracker_slot, TodoTrackerSlot::Present(_)));
        // 두 번째 take도 정상 동작 (이번엔 기존 인스턴스 반환)
        let _tracker2 = s.try_take_todo_tracker(ChannelId::new(42));
        assert!(_tracker2.is_some());
        assert!(matches!(s.todo_tracker_slot, TodoTrackerSlot::CheckedOut));
    }
}
