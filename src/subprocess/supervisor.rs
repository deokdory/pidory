use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use poise::serenity_prelude::{ChannelId, Context, MessageId};
use sqlx::PgPool;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinSet;
use tracing::{Instrument, info_span};

use crate::PendingPermission;
use crate::PendingQuestionGroup;
use crate::handler::reset_ui::PendingReset;
use crate::handler::session_state::SessionState;

use super::session_manager::SessionInner;

/// `(thread_id, cancel_flag)` 쌍을 값으로 갖는 recall 대기 맵 타입.
pub type PendingRecallMap = Arc<Mutex<HashMap<MessageId, (String, Arc<AtomicBool>)>>>;

/// 7개 핸들의 Arc clone을 담는 경량 구조체. Data struct 전체 참조를 피한다.
/// (#266 Stage 1) thread_id 키 9개 맵을 SessionState 단일 핸들로 통합 후 슬림화됨.
#[derive(Clone)]
pub struct SessionCleanupHandles {
    pub pending_permissions: Arc<Mutex<HashMap<String, PendingPermission>>>,
    pub pending_question_groups: Arc<Mutex<HashMap<String, PendingQuestionGroup>>>,
    pub pending_resets: Arc<Mutex<HashMap<String, PendingReset>>>,
    pub session_states: Arc<Mutex<HashMap<String, SessionState>>>,
    pub pending_recalls: PendingRecallMap,
    pub dispatch_locks: Arc<crate::handler::dispatch_locks::ThreadDispatchLocks>,
}

impl SessionCleanupHandles {
    pub fn from_data(data: &crate::Data) -> Self {
        // pending_recalls는 placeholder. SessionManager::get_or_create가 실제 Arc로 덮어쓴다.
        let placeholder = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        Self::from_parts(data, placeholder)
    }

    pub fn from_parts(
        data: &crate::Data,
        pending_recalls: PendingRecallMap,
    ) -> Self {
        Self {
            pending_permissions: Arc::clone(&data.pending_permissions),
            pending_question_groups: Arc::clone(&data.pending_question_groups),
            pending_resets: Arc::clone(&data.pending_resets),
            session_states: Arc::clone(&data.session_states),
            pending_recalls,
            dispatch_locks: Arc::clone(&data.dispatch_locks),
        }
    }

    #[cfg(test)]
    pub fn empty_for_test() -> Self {
        use std::collections::HashMap;
        Self {
            pending_permissions: Arc::new(Mutex::new(HashMap::new())),
            pending_question_groups: Arc::new(Mutex::new(HashMap::new())),
            pending_resets: Arc::new(Mutex::new(HashMap::new())),
            session_states: Arc::new(Mutex::new(HashMap::new())),
            pending_recalls: Arc::new(Mutex::new(HashMap::new())),
            dispatch_locks: Arc::new(crate::handler::dispatch_locks::ThreadDispatchLocks::new()),
        }
    }
}

/// core: DB/Discord 없이 테스트 가능한 분리된 정리 로직.
/// sessions HashMap에서 tid를 remove한다. 이미 None이면 다른 정리 경로가 처리했으므로 None 반환.
/// 반환된 SessionInner의 child는 호출자가 kill_with_timeout 처리.
/// 두 번째 반환값은 remove 후 남은 세션 수 — 추가 lock 없이 session_count_tx 업데이트 가능.
pub(super) async fn trigger_cleanup_core(
    sessions: &Arc<Mutex<HashMap<String, SessionInner>>>,
    thread_id: &str,
) -> (Option<SessionInner>, usize) {
    let mut ss = sessions.lock().await;
    let removed = ss.remove(thread_id);
    let len = ss.len();
    (removed, len)
    // lock 자동 drop (함수 종료).
}

/// 완전 wrapper: panic/cancel 감지, core 호출, child kill timeout, DB/Discord 알림, 13개 맵 정리.
/// NOTE: child.kill은 lock 해제 후 호출 — lock은 trigger_cleanup_core 내부에서 이미 drop됨.
///
/// `ready_rx`: get_or_create가 sessions.insert 완료 후 send하는 oneshot 채널.
/// supervisor는 이 신호를 받은 뒤에 worker/permission task를 관찰하기 시작한다.
/// sender가 drop(즉 get_or_create 실패 경로)되어도 Err를 무시하고 계속 진행한다 —
/// 어느 쪽이든 session이 맵에 없으면 do_cleanup이 early-exit하므로 안전하다.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_supervisor(
    thread_id: String,
    ready_rx: tokio::sync::oneshot::Receiver<()>,
    worker_fut: impl Future<Output = ()> + Send + 'static,
    permission_fut: impl Future<Output = ()> + Send + 'static,
    sessions: Arc<Mutex<HashMap<String, SessionInner>>>,
    handles: SessionCleanupHandles,
    db: PgPool,
    ctx: Context,
    notification_channel: Option<ChannelId>,
    session_count_tx: watch::Sender<usize>,
) {
    // sessions.insert가 완료될 때까지 대기 (spawn-before-insert race 방지).
    // sender drop(실패 경로) 시 Err → 무시하고 계속 진행. session이 맵에 없으면
    // do_cleanup이 early-exit하므로 어느 쪽이든 안전하다.
    let _ = ready_rx.await;

    let mut js: JoinSet<()> = JoinSet::new();
    js.spawn(worker_fut.instrument(info_span!("worker", thread_id = %thread_id)));
    js.spawn(permission_fut.instrument(info_span!("permission", thread_id = %thread_id)));

    while let Some(res) = js.join_next().await {
        match res {
            Err(e) if e.is_panic() => {
                tracing::error!(thread_id = %thread_id, error = %e, "session task panicked — triggering cleanup");
                // sibling task들을 먼저 abort + drain — cleanup 중 state 재생성 방지.
                // abort된 task는 Err(cancelled)를 빠르게 반환하므로 무한 대기 없음.
                js.abort_all();
                while js.join_next().await.is_some() {}
                do_cleanup(
                    &thread_id,
                    &sessions,
                    &handles,
                    &db,
                    &ctx,
                    notification_channel,
                    &session_count_tx,
                ).await;
                break;
            }
            Err(e) if e.is_cancelled() => {
                tracing::debug!(thread_id = %thread_id, "session task cancelled (normal abort path)");
                // JoinSet 내 다른 task도 곧 취소될 것. 반복 계속해서 다음 결과 소비.
            }
            Ok(()) => {
                tracing::debug!(thread_id = %thread_id, "session task ended normally");
                // 하나 정상 종료 → 나머지는 곧 자연 종료. 특별한 정리 불필요.
            }
            Err(e) => {
                tracing::warn!(thread_id = %thread_id, error = %e, "session task join error (neither panic nor cancelled)");
            }
        }
    }
    // JoinSet drop 시 나머지 task abort.
}

async fn do_cleanup(
    thread_id: &str,
    sessions: &Arc<Mutex<HashMap<String, SessionInner>>>,
    handles: &SessionCleanupHandles,
    db: &PgPool,
    ctx: &Context,
    notification_channel: Option<ChannelId>,
    session_count_tx: &watch::Sender<usize>,
) {
    // 1. sessions에서 remove (idempotency — None이면 이미 처리됨)
    let (removed, new_len) = trigger_cleanup_core(sessions, thread_id).await;
    let Some(mut inner) = removed else {
        tracing::debug!(thread_id = %thread_id, "session already removed by another path — cleanup skipped");
        return;
    };

    // session_count 업데이트 (trigger_cleanup_core에서 lock 안에 계산 — 추가 lock 불필요)
    let _ = session_count_tx.send(new_len);

    // pending_recalls도 정리 (이 세션이 소유했던 항목만)
    handles
        .pending_recalls
        .lock()
        .await
        .retain(|_, (tid, _)| tid != thread_id);

    // 2. child kill with timeout
    super::session_manager::kill_with_timeout(&mut inner.child).await;

    // 3. DB status update → error
    if let Err(e) = sqlx::query(
        "UPDATE sessions SET status = 'error' WHERE thread_id = $1",
    )
    .bind(thread_id)
    .execute(db)
    .await
    {
        tracing::warn!(thread_id = %thread_id, error = %e, "failed to update session status to error");
    }

    // 4. 13개 맵 정리 (handler::cleanup)
    crate::handler::cleanup::cleanup_session_state_from_handles(handles, thread_id, ctx).await;

    // 5. Discord 알림 (optional)
    if let Some(channel) = notification_channel
        && let Err(e) = channel
            .say(ctx, format!("⚠️ 세션 `{}`이 예기치 않게 종료됐어. 정리 완료.", thread_id))
            .await
    {
        tracing::warn!(thread_id = %thread_id, error = %e, "failed to send session panic notification");
    }
}
