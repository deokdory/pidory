use poise::serenity_prelude::Context;

use crate::Data;
use crate::subprocess::supervisor::SessionCleanupHandles;

/// Cleans up all in-memory state associated with a session.
///
/// Does NOT kill the subprocess (caller is responsible) and does NOT touch the DB.
/// `kill_session` failure should be ignored by the caller — the process may have already exited.
///
/// # Lock ordering
/// When holding both `Data.sessions` and `Data.session_states` locks, always acquire
/// `sessions` first, then `session_states`. Reverse order risks deadlock.
pub async fn cleanup_session_state(data: &Data, thread_id: &str, ctx: &Context) {
    data.pending_permissions
        .lock()
        .await
        .retain(|_, p| p.thread_id != thread_id);
    data.pending_question_groups
        .lock()
        .await
        .retain(|_, g| g.thread_id != thread_id);
    data.pending_resets
        .lock()
        .await
        .retain(|_, r| r.thread_id != thread_id);
    data.session_states.lock().await.remove(thread_id);
    data.dispatch_locks.remove(thread_id).await;
    let tracker = data.todo_trackers.lock().await.remove(thread_id);
    if let Some(tracker) = tracker {
        tracker.lock().await.cleanup(ctx).await;
    }

    // Leave the thread — the member list now signals session liveness
    if let Ok(id) = thread_id.parse::<u64>() {
        poise::serenity_prelude::ChannelId::new(id)
            .leave_thread(ctx)
            .await
            .ok();
    }
}

/// Cleans up all in-memory state associated with a session, given pre-cloned Arc handles.
///
/// Called by the supervisor when a worker panics and `Data` is not directly available.
/// Does NOT kill the subprocess and does NOT touch the DB.
/// `pending_recalls` is NOT cleaned up here — it is handled separately by the supervisor.
///
/// # Lock ordering
/// When holding both `Data.sessions` and `Data.session_states` locks, always acquire
/// `sessions` first, then `session_states`. Reverse order risks deadlock.
pub async fn cleanup_session_state_from_handles(
    handles: &SessionCleanupHandles,
    thread_id: &str,
    ctx: &Context,
) {
    handles
        .pending_permissions
        .lock()
        .await
        .retain(|_, p| p.thread_id != thread_id);
    handles
        .pending_question_groups
        .lock()
        .await
        .retain(|_, g| g.thread_id != thread_id);
    handles
        .pending_resets
        .lock()
        .await
        .retain(|_, r| r.thread_id != thread_id);
    handles.session_states.lock().await.remove(thread_id);
    handles.dispatch_locks.remove(thread_id).await;
    let tracker = handles.todo_trackers.lock().await.remove(thread_id);
    if let Some(tracker) = tracker {
        tracker.lock().await.cleanup(ctx).await;
    }

    // Leave the thread — the member list now signals session liveness
    if let Ok(id) = thread_id.parse::<u64>() {
        poise::serenity_prelude::ChannelId::new(id)
            .leave_thread(ctx)
            .await
            .ok();
    }
}
