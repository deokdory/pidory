use poise::serenity_prelude::Context;

use crate::Data;

/// Cleans up all in-memory state associated with a session.
///
/// Does NOT kill the subprocess (caller is responsible) and does NOT touch the DB.
/// `kill_session` failure should be ignored by the caller — the process may have already exited.
pub async fn cleanup_session_state(data: &Data, thread_id: &str, ctx: &Context) {
    data.session_skills.lock().await.remove(thread_id);
    data.next_step_buttons.lock().await.remove(thread_id);
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
    data.needs_context.lock().await.remove(thread_id);
    data.turn_initiators.lock().await.remove(thread_id);
    data.turn_participants.lock().await.remove(thread_id);
    data.last_tool_name.lock().await.remove(thread_id);
    data.kick_cooldowns.lock().await.remove(thread_id);
    data.kick_pending.lock().await.remove(thread_id);
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
