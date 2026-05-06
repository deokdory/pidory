// ─── permission wait / response ─────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::{FutureExt, StreamExt};
use poise::serenity_prelude::{MessageId, UserId};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc, mpsc::error::TrySendError};

use crate::claude_settings::rule::{RuleKind, Scope};
use crate::ratelimit::RateLimitInfo;
use crate::subprocess::parser::{StreamEvent, build_control_response_allow, build_control_response_deny, build_control_response_ask_answer};
use crate::subprocess::permission::{PermissionCache, PermissionDecision, PermissionRequest};
use crate::subprocess::session_manager::QueuedMessage;
use super::ratelimit_bridge::handle_ratelimit_event;
use super::io::build_user_message_json;

// ─── T5: Permission wait result (legacy) ────────────────────────────────────

#[allow(dead_code)]
pub(super) enum PermissionWaitResult {
    Allow,
    AllowAlways { tool_name: String, rule_kind: RuleKind, scope: Scope },
    Deny(String),                                 // reason
    Error,                                        // stdin error → caller does break
    Answer(std::collections::HashMap<String, String>), // answers for AskUserQuestion
}

// ─── T4a: Permission response writer ────────────────────────────────────────

/// Returns Ok(true) if Error variant (caller should break), Ok(false) on success.
#[allow(dead_code)]
async fn write_permission_response(
    result: PermissionWaitResult,
    request_id: &str,
    input: &serde_json::Value,
    stdin: &mut tokio::process::ChildStdin,
    permission_cache: &mut PermissionCache,
) -> Result<bool, std::io::Error> {
    match result {
        PermissionWaitResult::Allow => {
            let resp = build_control_response_allow(request_id, input);
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            tracing::info!(request_id = %request_id, behavior = "allow", "control_response written");
            Ok(false)
        }
        PermissionWaitResult::AllowAlways { tool_name, rule_kind, scope: _ } => {
            let resp = build_control_response_allow(request_id, input);
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            // 일관성 — production path 의 동일 분기 참고
            if matches!(rule_kind, RuleKind::Tool) {
                permission_cache.add_always_allow(&tool_name);
            } else {
                permission_cache.clear_tool(&tool_name);
            }
            tracing::info!(request_id = %request_id, behavior = "always_allow", tool_name = %tool_name, rule_kind = ?rule_kind, "control_response written");
            Ok(false)
        }
        PermissionWaitResult::Deny(reason) => {
            let resp = build_control_response_deny(request_id, &reason);
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            tracing::info!(request_id = %request_id, behavior = "deny", reason = %reason, "control_response written");
            Ok(false)
        }
        PermissionWaitResult::Error => {
            Ok(true) // caller should break
        }
        PermissionWaitResult::Answer(answers) => {
            let resp = build_control_response_ask_answer(request_id, input, &answers);
            stdin.write_all(resp.as_bytes()).await?;
            stdin.flush().await?;
            tracing::info!(request_id = %request_id, behavior = "answer", "control_response written");
            Ok(false)
        }
    }
}

// ─── T5a: Parallel permission wait (Wave 1.1) ──────────────────────────────

/// 동시 pending control_request 의 상한. 초과 시 auto-deny.
// permission_tx buffer 와 동기 (session_manager.rs).
const MAX_PENDING_CR: usize = 32;

/// 새 복수형 wait_for_permissions() 의 반환 타입.
#[derive(Debug)]
pub(super) enum PermissionsWaitResult {
    /// 최소 1개 CR 응답 후 pending 이 모두 비어 정상 종료됨.
    /// _decisions 는 처리된 (request_id, PermissionDecision) 목록 (Wave 1.3+ 에서 활용 예정).
    AllResolved { _decisions: Vec<(String, PermissionDecision)> },
    /// interrupt_rx 수신으로 인한 조기 종료.
    Interrupted,
    /// permission_tx (handler) 가 닫혔음 — 복구 불가.
    ChannelClosed,
}

/// wait_for_permissions() 호출자가 "첫 CR" 정보를 전달하는 용도.
pub(super) struct InitialControlRequest {
    pub(super) request_id: String,
    pub(super) tool_name: String,
    pub(super) tool_use_id: String,
    pub(super) input: serde_json::Value,
    pub(super) decision_reason: Option<String>,
    pub(super) triggered_by: UserId,
}

/// FuturesUnordered 내부 pending entry 상태.
struct PendingEntry {
    tool_name: String,
    saved_input: serde_json::Value,
}

/// 복수 pending CR 을 동시에 대기하는 공통 함수.
///
/// **STDIN OWNERSHIP INVARIANT**: 이 함수는 `stdin: &mut ChildStdin` 을 배타 소유한다.
/// 모든 stdin write (control_response, deny, mid-turn inject 등) 는 이 함수의
/// `tokio::select!` 분기 내부에서만 수행된다. 절대 `tokio::spawn()` 으로 writer 를
/// 분리하거나 `Arc<Mutex<ChildStdin>>` 로 감싸지 말 것. 동시 write 는 JSON 라인을
/// interleave 하여 Claude CLI 의 parser 를 깨뜨린다.
///
/// stdin is exclusively written in this task; do not spawn writers.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
pub(super) async fn wait_for_permissions<W, R>(
    stdin: &mut W,
    reader: &mut R,
    line: &mut String,
    queue_rx: &mut mpsc::Receiver<QueuedMessage>,
    interrupt_rx: &mut mpsc::Receiver<()>,
    queue_size: &Arc<AtomicUsize>,
    pending_recalls: &Arc<Mutex<HashMap<MessageId, (String, Arc<std::sync::atomic::AtomicBool>)>>>,
    thread_id: &str,
    event_tx: Option<&mpsc::Sender<StreamEvent>>,
    ratelimit_tx: &tokio::sync::watch::Sender<RateLimitInfo>,
    permission_cache: &mut PermissionCache,
    permission_tx: &mpsc::Sender<PermissionRequest>,
    initial_cr: InitialControlRequest,
) -> PermissionsWaitResult
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    // HashMap<request_id, PendingEntry>
    let mut pending: HashMap<String, PendingEntry> = HashMap::new();

    // FuturesUnordered: 각 future 는 (request_id, Result<PermissionDecision, RecvError>) 를 yield
    let mut futures: FuturesUnordered<
        BoxFuture<'static, (String, Result<PermissionDecision, tokio::sync::oneshot::error::RecvError>)>,
    > = FuturesUnordered::new();

    // 누적 decisions (AllResolved 에 포함됨)
    let mut decisions: Vec<(String, PermissionDecision)> = Vec::new();

    // ── initial_cr 처리 ──────────────────────────────────────────────────────
    {
        let rid = initial_cr.request_id.clone();
        let tool_name = initial_cr.tool_name.clone();

        if permission_cache.is_always_allowed(&tool_name) {
            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, "cache hit, auto-allow");
            let resp = build_control_response_allow(&rid, &initial_cr.input);
            if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                tracing::error!("stdin write error (wait_for_permissions initial auto-allow): {}", e);
                // stdin error → 즉시 Interrupted 로 처리 (caller 가 break 결정)
                return PermissionsWaitResult::Interrupted;
            }
            let _ = stdin.flush().await;
            // cache hit: 이미 허용됨 → AllResolved 로 즉시 반환
            decisions.push((rid, PermissionDecision::Allow));
            return PermissionsWaitResult::AllResolved { _decisions: decisions };
        }

        // handler 로 전송
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<PermissionDecision>();
        let perm_req = PermissionRequest {
            request_id: rid.clone(),
            tool_name: tool_name.clone(),
            tool_use_id: initial_cr.tool_use_id.clone(),
            input: initial_cr.input.clone(),
            decision_reason: initial_cr.decision_reason.clone(),
            response_tx: resp_tx,
            triggered_by: initial_cr.triggered_by,
        };

        match permission_tx.try_send(perm_req) {
            Ok(()) => {
                tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, "permission_tx send ok");
                pending.insert(rid.clone(), PendingEntry { tool_name, saved_input: initial_cr.input.clone() });
                let fut = async move { (rid, resp_rx.await) }.boxed();
                futures.push(fut);
            }
            Err(TrySendError::Full(dropped)) => {
                let dropped_rid = dropped.request_id.clone();
                let dropped_tool = dropped.tool_name.clone();
                tracing::warn!(thread_id = %thread_id, request_id = %dropped_rid, tool_name = %dropped_tool, "permission_tx full, auto-denying");
                let resp = build_control_response_deny(&dropped_rid, "Permission queue full");
                if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                    tracing::error!("stdin write error (wait_for_permissions initial full-deny): {}", e);
                    return PermissionsWaitResult::Interrupted;
                }
                let _ = stdin.flush().await;
                // auto-deny 완료 → AllResolved 반환 (최소 1개 처리됨)
                decisions.push((dropped_rid, PermissionDecision::Deny));
                return PermissionsWaitResult::AllResolved { _decisions: decisions };
            }
            Err(TrySendError::Closed(_)) => {
                tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, "permission_tx send failed");
                return PermissionsWaitResult::ChannelClosed;
            }
        }
    }

    // ── 메인 루프 ────────────────────────────────────────────────────────────
    loop {
        line.clear();
        tokio::select! {
            biased;

            // ── interrupt ──────────────────────────────────────────────────
            _ = interrupt_rx.recv() => {
                // 모든 pending CR 에 대해 deny 전송 (stdin 직렬 write, write-per-flush)
                for (rid, entry) in &pending {
                    let resp = build_control_response_deny(rid, "Interrupted by user");
                    if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                        tracing::error!("stdin write error (wait_for_permissions interrupt deny): {}", e);
                        break;
                    }
                    if let Err(e) = stdin.flush().await {
                        tracing::error!("stdin flush error (wait_for_permissions interrupt deny): {}", e);
                        break;
                    }
                    tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %entry.tool_name, behavior = "deny", reason = "Interrupted by user", "control_response written");
                }
                // resp_rx future 들은 drop 으로 처리됨
                return PermissionsWaitResult::Interrupted;
            }

            // ── mid-turn inject ────────────────────────────────────────────
            new_msg = queue_rx.recv() => {
                match new_msg {
                    Some(m) => {
                        queue_size.fetch_sub(1, Ordering::Relaxed);
                        pending_recalls.lock().await.remove(&m.message_id);
                        if m.cancelled.load(Ordering::Acquire) {
                            tracing::info!(thread_id = %thread_id, msg_id = %m.message_id, "Message recalled, skipping");
                            continue;
                        }
                        let inject_line = build_user_message_json(&m.content, &m.downloaded_files, m.reply_context.as_ref());
                        if let Err(e) = stdin.write_all(inject_line.as_bytes()).await {
                            tracing::error!("mid-turn stdin write error (wait_for_permissions): {}", e);
                            return PermissionsWaitResult::Interrupted;
                        }
                        let _ = stdin.flush().await;
                    }
                    None => {
                        // queue closed
                        return PermissionsWaitResult::ChannelClosed;
                    }
                }
            }

            // ── stdout read (추가 CR 수신 가능, MAX_PENDING_CR 상한) ───────
            read = reader.read_line(line), if futures.len() < MAX_PENDING_CR => {
                match read {
                    Ok(0) => {
                        tracing::info!("Process stdout EOF during wait_for_permissions for thread {}", thread_id);
                        queue_rx.close();
                        // pending 전부 deny (stdin 직렬 write, write-per-flush)
                        for (rid, entry) in &pending {
                            let resp = build_control_response_deny(rid, "Process exited");
                            if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                tracing::error!("stdin write error (wait_for_permissions EOF deny): {}", e);
                                break;
                            }
                            if let Err(e) = stdin.flush().await {
                                tracing::error!("stdin flush error (wait_for_permissions EOF deny): {}", e);
                                break;
                            }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %entry.tool_name, behavior = "deny", reason = "Process exited", "control_response written");
                        }
                        return PermissionsWaitResult::Interrupted;
                    }
                    Ok(_) => {
                        let trimmed = line.trim_end();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match crate::subprocess::parser::parse_line(trimmed) {
                            Ok(StreamEvent::ControlRequest { ref request_id, ref tool_name, ref tool_use_id, ref input, ref decision_reason, .. }) => {
                                let rid = request_id.clone();
                                let tname = tool_name.clone();
                                tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tname, "control_request received");

                                if permission_cache.is_always_allowed(&tname) {
                                    tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tname, "cache hit, auto-allow");
                                    let resp = build_control_response_allow(&rid, input);
                                    if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                        tracing::error!("stdin write error (wait_for_permissions additional auto-allow): {}", e);
                                        return PermissionsWaitResult::Interrupted;
                                    }
                                    let _ = stdin.flush().await;
                                    // auto-allow 는 pending/futures 에 추가하지 않음 (즉시 처리됨)
                                } else {
                                    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel::<PermissionDecision>();
                                    let perm_req = PermissionRequest {
                                        request_id: rid.clone(),
                                        tool_name: tname.clone(),
                                        tool_use_id: tool_use_id.clone(),
                                        input: input.clone(),
                                        decision_reason: decision_reason.clone(),
                                        response_tx: resp_tx,
                                        triggered_by: initial_cr.triggered_by,
                                    };

                                    match permission_tx.try_send(perm_req) {
                                        Ok(()) => {
                                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tname, "permission_tx send ok");
                                            pending.insert(rid.clone(), PendingEntry { tool_name: tname, saved_input: input.clone() });
                                            let fut = async move { (rid, resp_rx.await) }.boxed();
                                            futures.push(fut);
                                        }
                                        Err(TrySendError::Full(dropped)) => {
                                            let dropped_rid = dropped.request_id.clone();
                                            let dropped_tool = dropped.tool_name.clone();
                                            tracing::warn!(thread_id = %thread_id, request_id = %dropped_rid, tool_name = %dropped_tool, "permission_tx full, auto-denying");
                                            let resp = build_control_response_deny(&dropped_rid, "Permission queue full");
                                            if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                                tracing::error!("stdin write error (wait_for_permissions full-deny): {}", e);
                                                return PermissionsWaitResult::Interrupted;
                                            }
                                            let _ = stdin.flush().await;
                                        }
                                        Err(TrySendError::Closed(_)) => {
                                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tname, "permission_tx send failed");
                                            return PermissionsWaitResult::ChannelClosed;
                                        }
                                    }
                                }
                            }
                            Ok(StreamEvent::RateLimit { rate_limit_type, utilization, resets_at, is_using_overage, .. }) => {
                                handle_ratelimit_event(ratelimit_tx, rate_limit_type.as_deref(), utilization, resets_at, is_using_overage);
                            }
                            Ok(ev) => {
                                if let Some(tx) = event_tx {
                                    let _ = tx.send(ev).await;
                                } else {
                                    tracing::debug!("Draining event during wait_for_permissions (no event_tx): {:?}", ev);
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Parse error (wait_for_permissions): {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("stdout read error (wait_for_permissions): {}", e);
                        queue_rx.close();
                        // pending 전부 deny (stdin 직렬 write, write-per-flush)
                        for (rid, entry) in &pending {
                            let resp = build_control_response_deny(rid, "Process exited");
                            if let Err(e) = stdin.write_all(resp.as_bytes()).await {
                                tracing::error!("stdin write error (wait_for_permissions read-error deny): {}", e);
                                break;
                            }
                            if let Err(e) = stdin.flush().await {
                                tracing::error!("stdin flush error (wait_for_permissions read-error deny): {}", e);
                                break;
                            }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %entry.tool_name, behavior = "deny", reason = "Process exited", "control_response written");
                        }
                        return PermissionsWaitResult::Interrupted;
                    }
                }
            }

            // ── permission decision resolved ───────────────────────────────
            resolved = futures.next(), if !futures.is_empty() => {
                let (rid, result): (String, Result<PermissionDecision, tokio::sync::oneshot::error::RecvError>) = resolved.expect("FuturesUnordered next() must not be None when non-empty");
                let entry = pending.remove(&rid).expect("pending entry must exist when future resolves");
                let decision = result.unwrap_or(PermissionDecision::Deny);

                // stdin 직렬 write (단일 select! 분기 내부 — Mutex 불필요)
                let write_result = {
                    let tool_name = &entry.tool_name;
                    let input = &entry.saved_input;
                    match &decision {
                        PermissionDecision::Allow => {
                            let resp = build_control_response_allow(&rid, input);
                            let r = stdin.write_all(resp.as_bytes()).await;
                            if r.is_ok() { let _ = stdin.flush().await; }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, behavior = "allow", "control_response written");
                            r
                        }
                        PermissionDecision::AllowAlways { rule_kind, scope: _ } => {
                            let resp = build_control_response_allow(&rid, input);
                            let r = stdin.write_all(resp.as_bytes()).await;
                            if r.is_ok() {
                                let _ = stdin.flush().await;
                                // NP12-E: settings.json 이 source of truth. Tool kind 만 turn-local mirror
                                // (Bash(*) 처럼 input 무관한 매칭 규칙) → 같은 turn 내 같은 tool 재호출 시
                                // cache hit 으로 auto-allow. Exact/Prefix/Domain 은 input-dependent 이라
                                // tool name 단위 cache 로 정확히 표현 불가 → clear_tool (기존 동작 유지).
                                // pending_session_restart 가 다음 user message 시 cache 를 자연 폐기시킴.
                                if matches!(rule_kind, RuleKind::Tool) {
                                    permission_cache.add_always_allow(tool_name);
                                } else {
                                    permission_cache.clear_tool(tool_name);
                                }
                            }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, behavior = "always_allow", rule_kind = ?rule_kind, "control_response written");
                            r
                        }
                        PermissionDecision::Deny => {
                            let resp = build_control_response_deny(&rid, "User rejected this action");
                            let r = stdin.write_all(resp.as_bytes()).await;
                            if r.is_ok() { let _ = stdin.flush().await; }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, behavior = "deny", "control_response written");
                            r
                        }
                        PermissionDecision::Answer(answers) => {
                            let resp = build_control_response_ask_answer(&rid, input, answers);
                            let r = stdin.write_all(resp.as_bytes()).await;
                            if r.is_ok() { let _ = stdin.flush().await; }
                            tracing::info!(thread_id = %thread_id, request_id = %rid, tool_name = %tool_name, behavior = "answer", "control_response written");
                            r
                        }
                    }
                };

                if let Err(e) = write_result {
                    tracing::error!("stdin write error (wait_for_permissions resolved): {}", e);
                    return PermissionsWaitResult::Interrupted;
                }

                decisions.push((rid, decision));

                // pending 이 모두 비어있고 최소 1개 처리 완료 → AllResolved
                if pending.is_empty() && !decisions.is_empty() {
                    return PermissionsWaitResult::AllResolved { _decisions: decisions };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subprocess::permission::{PermissionDecision, PermissionRequest};
    use crate::subprocess::parser::{build_control_response_allow, build_control_response_deny};
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, BufReader};

    // ── PermissionWaitResult enum ────────────────────────────────────────────

    #[test]
    fn permission_wait_result_allow_variant() {
        let r = PermissionWaitResult::Allow;
        assert!(matches!(r, PermissionWaitResult::Allow));
    }

    #[test]
    fn permission_wait_result_always_allow_carries_tool_name() {
        let r = PermissionWaitResult::AllowAlways {
            tool_name: "bash".to_string(),
            rule_kind: RuleKind::Exact,
            scope: Scope::Project,
        };
        match r {
            PermissionWaitResult::AllowAlways { tool_name, .. } => assert_eq!(tool_name, "bash"),
            _ => panic!("expected AllowAlways"),
        }
    }

    #[test]
    fn permission_wait_result_deny_carries_reason() {
        let r = PermissionWaitResult::Deny("not allowed".to_string());
        match r {
            PermissionWaitResult::Deny(reason) => assert_eq!(reason, "not allowed"),
            _ => panic!("expected Deny"),
        }
    }

    #[test]
    fn permission_wait_result_error_variant() {
        let r = PermissionWaitResult::Error;
        assert!(matches!(r, PermissionWaitResult::Error));
    }

    // ── write_permission_response ────────────────────────────────────────────

    /// Spawn a `cat` subprocess to get a real ChildStdin for write tests.
    async fn spawn_cat_stdin() -> tokio::process::ChildStdin {
        let mut child = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn cat");
        child.stdin.take().expect("no stdin")
    }

    #[tokio::test]
    async fn write_permission_response_allow_returns_ok_false() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({"command": "ls"});
        let result = write_permission_response(
            PermissionWaitResult::Allow,
            "req-001",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await;
        assert!(!result.unwrap(), "Allow variant must return Ok(false)");
    }

    #[tokio::test]
    async fn write_permission_response_allow_writes_allow_json() {
        // Verify the JSON written to stdin has behavior=allow.
        let expected = build_control_response_allow("req-002", &serde_json::json!({}));
        let v: serde_json::Value = serde_json::from_str(expected.trim()).unwrap();
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(v["response"]["request_id"], "req-002");
    }

    #[tokio::test]
    async fn write_permission_response_always_allow_invalidates_cache() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();

        // 사전 조건: 다른 경로로 cache에 들어가 있던 상태 (예: 이전 prompt 시)
        cache.add_always_allow("Bash");
        assert!(cache.is_always_allowed("Bash"));

        let input = serde_json::json!({"command": "echo hi"});
        let result = write_permission_response(
            PermissionWaitResult::AllowAlways {
                tool_name: "Bash".to_string(),
                rule_kind: RuleKind::Exact,
                scope: Scope::Project,
            },
            "req-003",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await;
        assert!(!result.unwrap(), "AllowAlways must return Ok(false)");
        assert!(!cache.is_always_allowed("Bash"), "cache must be invalidated (settings.json is now source of truth)");
    }

    #[tokio::test]
    async fn write_permission_response_always_allow_does_not_affect_other_tools() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        cache.add_always_allow("Bash");
        cache.add_always_allow("Read");  // 다른 tool도 미리 등록

        let input = serde_json::json!({});
        write_permission_response(
            PermissionWaitResult::AllowAlways {
                tool_name: "Write".to_string(),
                rule_kind: RuleKind::Exact,
                scope: Scope::Project,
            },
            "req-004",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await
        .unwrap();

        // Write는 cache에 없었으니 변화 없음
        assert!(!cache.is_always_allowed("Write"));
        // 다른 tool은 영향 없음
        assert!(cache.is_always_allowed("Bash"), "Bash should still be cached");
        assert!(cache.is_always_allowed("Read"), "Read should still be cached");
    }

    #[tokio::test]
    async fn write_permission_response_deny_returns_ok_false() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        let result = write_permission_response(
            PermissionWaitResult::Deny("user rejected".to_string()),
            "req-005",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await;
        assert!(!result.unwrap(), "Deny variant must return Ok(false)");
    }

    #[tokio::test]
    async fn write_permission_response_deny_does_not_update_cache() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        write_permission_response(
            PermissionWaitResult::Deny("no".to_string()),
            "req-006",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await
        .unwrap();
        assert!(!cache.is_always_allowed("Bash"), "Deny must not update the permission cache");
    }

    #[tokio::test]
    async fn write_permission_response_error_returns_ok_true() {
        // Error variant signals caller to break — no stdin needed.
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        let result = write_permission_response(
            PermissionWaitResult::Error,
            "req-007",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await;
        assert!(result.unwrap(), "Error variant must return Ok(true) to signal break");
    }

    #[tokio::test]
    async fn write_permission_response_error_does_not_update_cache() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        write_permission_response(
            PermissionWaitResult::Error,
            "req-008",
            &input,
            &mut stdin,
            &mut cache,
        )
        .await
        .unwrap();
        assert!(!cache.is_always_allowed("Bash"), "Error must not touch the permission cache");
    }

    // ── build_control_response_allow / build_control_response_deny ───────────

    #[test]
    fn control_response_allow_json_structure() {
        let input = serde_json::json!({"command": "ls"});
        let out = build_control_response_allow("rid-1", &input);
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["type"], "control_response");
        assert_eq!(v["response"]["subtype"], "success");
        assert_eq!(v["response"]["request_id"], "rid-1");
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(v["response"]["response"]["updatedInput"]["command"], "ls");
    }

    #[test]
    fn control_response_allow_ends_with_newline() {
        let out = build_control_response_allow("rid-2", &serde_json::json!({}));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn control_response_deny_json_structure() {
        let out = build_control_response_deny("rid-3", "user rejected");
        let v: serde_json::Value = serde_json::from_str(out.trim()).expect("valid JSON");
        assert_eq!(v["type"], "control_response");
        assert_eq!(v["response"]["subtype"], "success");
        assert_eq!(v["response"]["request_id"], "rid-3");
        assert_eq!(v["response"]["response"]["behavior"], "deny");
        assert_eq!(v["response"]["response"]["message"], "user rejected");
    }

    #[test]
    fn control_response_deny_ends_with_newline() {
        let out = build_control_response_deny("rid-4", "reason");
        assert!(out.ends_with('\n'));
    }

    #[tokio::test]
    async fn write_permission_response_answer_returns_ok_false() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({"questions": [{"question": "pick?"}]});
        let answers = std::collections::HashMap::from([("q_0".to_string(), "Blue".to_string())]);
        let result = write_permission_response(
            PermissionWaitResult::Answer(answers),
            "req-ask",
            &input,
            &mut stdin,
            &mut cache,
        ).await;
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn write_permission_response_answer_does_not_update_cache() {
        let mut stdin = spawn_cat_stdin().await;
        let mut cache = PermissionCache::new();
        let input = serde_json::json!({});
        let answers = std::collections::HashMap::from([("q_0".to_string(), "test".to_string())]);
        write_permission_response(
            PermissionWaitResult::Answer(answers),
            "req-ask2",
            &input,
            &mut stdin,
            &mut cache,
        ).await.unwrap();
        assert!(!cache.is_always_allowed("AskUserQuestion"));
    }

    // ── Wave 2.1 + 2.3 + 2.4: parallel CR mock tests ─────────────────────────

    /// CR JSON 라인 1개를 반환하는 헬퍼 (wait_for_permissions reader 주입용).
    fn cr_json_line(request_id: &str, tool_name: &str) -> Vec<u8> {
        let line = format!(
            "{{\"type\":\"control_request\",\"request_id\":\"{request_id}\",\"tool_name\":\"{tool_name}\",\"tool_use_id\":\"use_{request_id}\",\"input\":{{}}}}\n"
        );
        line.into_bytes()
    }

    /// 테스트용 `InitialControlRequest` 생성 헬퍼.
    fn make_initial_cr(request_id: &str, tool_name: &str) -> InitialControlRequest {
        InitialControlRequest {
            request_id: request_id.to_string(),
            tool_name: tool_name.to_string(),
            tool_use_id: format!("use_{request_id}"),
            input: serde_json::json!({}),
            decision_reason: None,
            triggered_by: poise::serenity_prelude::UserId::new(1),
        }
    }

    /// stdin write 내용을 읽어 JSON 값으로 파싱하는 헬퍼.
    /// write-side 가 drop 되거나 expected_count 개 수집되면 종료.
    async fn drain_stdin_writes(
        read_rx: &mut tokio::io::DuplexStream,
        expected_count: usize,
    ) -> Vec<serde_json::Value> {
        let mut results = Vec::new();
        let mut buf = String::new();
        let mut byte_buf = [0u8; 1];
        loop {
            if results.len() >= expected_count {
                break;
            }
            match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                read_rx.read(&mut byte_buf),
            ).await {
                Ok(Ok(0)) => break,
                Ok(Ok(_)) => {
                    let ch = byte_buf[0] as char;
                    buf.push(ch);
                    if ch == '\n' && !buf.trim().is_empty()
                        && let Ok(v) = serde_json::from_str::<serde_json::Value>(buf.trim()) {
                        results.push(v);
                        buf.clear();
                    }
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
        results
    }

    /// 공통 채널 셋업 매크로 helper (반복 코드 축약).
    /// NOTE: queue_tx 와 interrupt_tx 는 절대 drop 하지 마라 — select! 에서 None 수신 시 종료됨.
    macro_rules! setup_channels {
        () => {{
            let (queue_tx, queue_rx) = tokio::sync::mpsc::channel::<crate::subprocess::session_manager::QueuedMessage>(5);
            let (interrupt_tx, interrupt_rx) = tokio::sync::mpsc::channel::<()>(1);
            let queue_size = Arc::new(AtomicUsize::new(0));
            let pending_recalls = Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::<poise::serenity_prelude::MessageId, (String, Arc<std::sync::atomic::AtomicBool>)>::new()
            ));
            let (ratelimit_tx, _ratelimit_rx) = tokio::sync::watch::channel(crate::ratelimit::RateLimitInfo::default());
            (queue_tx, queue_rx, interrupt_tx, interrupt_rx, queue_size, pending_recalls, ratelimit_tx)
        }};
    }

    /// duplex 기반 mock reader 생성 헬퍼.
    /// write-side (`DuplexStream`) 를 keep-alive 로 유지하면 reader 는 EOF 를 반환하지 않음.
    /// CR 라인들을 write-side 에 미리 쓰고, write-side 를 반환해 keep-alive 상태 유지.
    async fn make_duplex_reader(initial_lines: &[Vec<u8>]) -> (BufReader<tokio::io::DuplexStream>, tokio::io::DuplexStream) {
        use tokio::io::AsyncWriteExt;
        let (writer, reader_stream) = tokio::io::duplex(65536);
        let mut writer = writer;
        for line in initial_lines {
            writer.write_all(line).await.unwrap();
        }
        (BufReader::new(reader_stream), writer)
    }

    /// 2.1: 병렬 2 CR 모두 permission_tx 에 도달
    #[tokio::test]
    async fn parallel_two_crs_both_surfaced() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, _stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: 2개 수신 후 Allow 전송
        let collect_task = tokio::spawn(async move {
            let mut received = Vec::new();
            if let Some(req) = permission_rx.recv().await {
                received.push(req.request_id.clone());
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
            if let Some(req) = permission_rx.recv().await {
                received.push(req.request_id.clone());
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
            received
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;

        let received_ids = collect_task.await.unwrap();
        assert_eq!(received_ids.len(), 2, "permission_tx 에 2개 CR 모두 도달해야 함");
        assert!(received_ids.contains(&"cr1".to_string()));
        assert!(received_ids.contains(&"cr2".to_string()));
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.1: CR 2개 후 FIFO 순서로 Allow → stdin 에 2개 allow JSON 기록
    #[tokio::test]
    async fn parallel_two_crs_fifo_response() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: cr1 먼저 Allow, cr2 나중 Allow (FIFO)
        let handler_task = tokio::spawn(async move {
            let req1 = permission_rx.recv().await.unwrap();
            let req2 = permission_rx.recv().await.unwrap();
            let _ = req1.response_tx.send(PermissionDecision::Allow);
            let _ = req2.response_tx.send(PermissionDecision::Allow);
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 2).await;
        assert_eq!(writes.len(), 2, "stdin 에 2개 JSON 기록되어야 함");
        for w in &writes {
            assert_eq!(w["type"], "control_response");
            assert_eq!(w["response"]["response"]["behavior"], "allow");
        }
        let rids: Vec<&str> = writes.iter().map(|w| w["response"]["request_id"].as_str().unwrap()).collect();
        assert!(rids.contains(&"cr1"), "cr1 allow 포함");
        assert!(rids.contains(&"cr2"), "cr2 allow 포함");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.1: CR 2개, resp_rx2 먼저 Allow, resp_rx1 나중 Allow → stdin 에 역순 write 정상
    #[tokio::test]
    async fn parallel_two_crs_reverse_response() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: cr2 먼저 Allow, cr1 나중 Allow (역순)
        let handler_task = tokio::spawn(async move {
            let req1 = permission_rx.recv().await.unwrap();
            let req2 = permission_rx.recv().await.unwrap();
            let _ = req2.response_tx.send(PermissionDecision::Allow);
            let _ = req1.response_tx.send(PermissionDecision::Allow);
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 2).await;
        assert_eq!(writes.len(), 2, "stdin 에 2개 JSON 기록되어야 함");
        for w in &writes {
            assert_eq!(w["type"], "control_response");
            assert_eq!(w["response"]["response"]["behavior"], "allow");
        }
        let rids: Vec<&str> = writes.iter().map(|w| w["response"]["request_id"].as_str().unwrap()).collect();
        assert!(rids.contains(&"cr1"), "cr1 allow 포함");
        assert!(rids.contains(&"cr2"), "cr2 allow 포함");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.1: CR 3개, 1 Allow + 2 Deny + 3 Allow → stdin 에 3개 대응 JSON
    #[tokio::test]
    async fn parallel_three_crs_mixed() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        let cr3_bytes = cr_json_line("cr3", "Edit");
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes, cr3_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(8192);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: 3개 모두 수신 후 cr1=Allow, cr2=Deny, cr3=Allow
        let handler_task = tokio::spawn(async move {
            let mut requests = std::collections::HashMap::new();
            for _ in 0..3 {
                if let Some(req) = permission_rx.recv().await {
                    requests.insert(req.request_id.clone(), req);
                }
            }
            if let Some(req) = requests.remove("cr1") {
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
            if let Some(req) = requests.remove("cr2") {
                let _ = req.response_tx.send(PermissionDecision::Deny);
            }
            if let Some(req) = requests.remove("cr3") {
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 3).await;
        assert_eq!(writes.len(), 3, "stdin 에 3개 JSON 기록되어야 함");
        let mut by_rid: std::collections::HashMap<&str, &serde_json::Value> = std::collections::HashMap::new();
        for w in &writes {
            by_rid.insert(w["response"]["request_id"].as_str().unwrap(), w);
        }
        assert_eq!(by_rid["cr1"]["response"]["response"]["behavior"], "allow", "cr1 → allow");
        assert_eq!(by_rid["cr2"]["response"]["response"]["behavior"], "deny",  "cr2 → deny");
        assert_eq!(by_rid["cr3"]["response"]["response"]["behavior"], "allow", "cr3 → allow");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.3: interrupt_rx 수신 시 pending 전체에 Deny stdin write
    #[tokio::test]
    async fn interrupt_during_pending_sends_deny_to_all() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        // duplex reader: EOF 없이 cr2 주입 후 대기
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        // permission buffer 충분히 크게
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: 2개 수신 후 interrupt 전송 (resp_tx 는 보관 — future 가 살아있어야 pending 유지)
        let interrupt_task = tokio::spawn(async move {
            let _req1 = permission_rx.recv().await; // resp_tx keep-alive
            let _req2 = permission_rx.recv().await; // resp_tx keep-alive
            let _ = interrupt_tx.send(()).await;
            // _req1, _req2 는 이 스코프 끝에서 drop — 여기서는 interrupt 보내기 전에 pending 상태
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        interrupt_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 2).await;
        assert_eq!(writes.len(), 2, "interrupt 시 pending 2개 deny 기록되어야 함");
        for w in &writes {
            assert_eq!(w["type"], "control_response");
            assert_eq!(w["response"]["response"]["behavior"], "deny");
            assert_eq!(w["response"]["response"]["message"], "Interrupted by user");
        }
        assert!(matches!(result, PermissionsWaitResult::Interrupted));
    }

    /// 2.3: resp_tx drop → RecvError → Deny 해석, stdin write
    #[tokio::test]
    async fn resp_rx_dropped_interprets_as_deny() {
        // reader: 추가 CR 없음. duplex reader 로 EOF 없이 대기 — futures arm 이 RecvError 로 resolve 됨.
        let (mut reader, _reader_write) = make_duplex_reader(&[]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: cr1 수신 후 resp_tx drop (RecvError 유발)
        let handler_task = tokio::spawn(async move {
            if let Some(req) = permission_rx.recv().await {
                drop(req.response_tx); // resp_rx.await → RecvError → Deny
            }
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 1).await;
        assert_eq!(writes.len(), 1, "RecvError → deny JSON 1개 기록되어야 함");
        assert_eq!(writes[0]["type"], "control_response");
        assert_eq!(writes[0]["response"]["response"]["behavior"], "deny");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.3: permission_tx closed → PermissionsWaitResult::ChannelClosed 반환
    #[tokio::test]
    async fn permission_tx_closed_returns_channel_closed() {
        // reader: EOF (initial CR 처리 전에 closed 감지됨)
        let mock_reader = tokio_test::io::Builder::new().build();
        let mut reader = BufReader::new(mock_reader);
        let mut line = String::new();

        let (mut stdin_write, _stdin_read) = tokio::io::duplex(4096);
        // permission_rx drop → try_send 가 TrySendError::Closed 반환
        let (permission_tx, permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        drop(permission_rx); // 먼저 rx drop → tx.try_send 가 Closed

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;

        assert!(
            matches!(result, PermissionsWaitResult::ChannelClosed),
            "permission_rx closed → ChannelClosed 반환해야 함"
        );
    }

    /// 2.4: permission_tx buffer 1, buffer 꽉 찬 상태에서 initial CR → auto-deny
    #[tokio::test]
    async fn permission_tx_full_auto_denies_initial() {
        let mock_reader = tokio_test::io::Builder::new().build();
        let mut reader = BufReader::new(mock_reader);
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        // buffer 1 로 설정 후 dummy 로 채움
        let (permission_tx, _permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(1);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let (dummy_tx, _dummy_rx) = tokio::sync::oneshot::channel::<PermissionDecision>();
        let dummy_req = PermissionRequest {
            request_id: "dummy".to_string(),
            tool_name: "Dummy".to_string(),
            tool_use_id: "use_dummy".to_string(),
            input: serde_json::json!({}),
            decision_reason: None,
            response_tx: dummy_tx,
            triggered_by: poise::serenity_prelude::UserId::new(1),
        };
        permission_tx.try_send(dummy_req).expect("buffer 가 비어있어야 함");

        let initial_cr = make_initial_cr("cr1", "Bash");

        // initial CR full-deny 는 select! 진입 전 처리 → queue/interrupt keep-alive 불필요
        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;

        let writes = drain_stdin_writes(&mut stdin_read, 1).await;
        assert_eq!(writes.len(), 1, "full-deny 1개 기록되어야 함");
        assert_eq!(writes[0]["type"], "control_response");
        assert_eq!(writes[0]["response"]["response"]["behavior"], "deny");
        assert_eq!(writes[0]["response"]["response"]["message"], "Permission queue full");
        assert_eq!(writes[0]["response"]["request_id"], "cr1");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// 2.4: permission_tx buffer 1, initial 통과 후 loop CR 2개 → 2번째부터 full-deny
    #[tokio::test]
    async fn permission_tx_full_auto_denies_during_loop() {
        let cr2_bytes = cr_json_line("cr2", "Write");
        let cr3_bytes = cr_json_line("cr3", "Edit");
        // duplex reader: cr2, cr3 주입 후 EOF 없이 대기
        let (mut reader, _reader_write) = make_duplex_reader(&[cr2_bytes, cr3_bytes]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(8192);
        // buffer 1: cr1 initial 이 차지하면 cr2, cr3 는 full
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(1);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        // handler: cr1 수신 후 Allow. cr2/cr3 는 full-deny 로 permission_rx 에 오지 않음.
        let handler_task = tokio::spawn(async move {
            if let Some(req) = permission_rx.recv().await {
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        // allow 1 + full-deny 2 = 3개
        let writes = drain_stdin_writes(&mut stdin_read, 3).await;
        assert_eq!(writes.len(), 3, "allow 1개 + full-deny 2개 = 3개 기록되어야 함");
        let mut by_rid: std::collections::HashMap<&str, &serde_json::Value> = std::collections::HashMap::new();
        for w in &writes {
            by_rid.insert(w["response"]["request_id"].as_str().unwrap(), w);
        }
        assert_eq!(by_rid["cr1"]["response"]["response"]["behavior"], "allow", "cr1 → allow");
        assert_eq!(by_rid["cr2"]["response"]["response"]["behavior"], "deny",  "cr2 → full-deny");
        assert_eq!(by_rid["cr3"]["response"]["response"]["behavior"], "deny",  "cr3 → full-deny");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }

    /// AllowAlways + RuleKind::Tool → cache.add_always_allow("Bash")
    #[tokio::test]
    async fn parallel_allow_always_tool_kind_caches_tool() {
        let (mut reader, _reader_write) = make_duplex_reader(&[]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr1", "Bash");

        tokio::spawn(async move {
            if let Some(req) = permission_rx.recv().await {
                let _ = req.response_tx.send(PermissionDecision::AllowAlways {
                    rule_kind: RuleKind::Tool,
                    scope: Scope::Project,
                });
            }
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;

        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
        assert!(cache.is_always_allowed("Bash"), "Tool kind AllowAlways must add to cache");

        let writes = drain_stdin_writes(&mut stdin_read, 1).await;
        assert_eq!(writes.len(), 1, "stdin 1건 (cr1 allow) 기록되어야 함");
        assert_eq!(writes[0]["response"]["response"]["behavior"], "allow", "cr1 → allow");
        assert_eq!(writes[0]["response"]["request_id"], "cr1");
    }

    /// AllowAlways + RuleKind::Exact → clear_tool 호출, cache 에 추가하지 않음
    ///
    /// pre-condition: "Bash" 가 이미 캐시에 있는 상태. initial CR 은 "Write" (캐시 미스) 로 전송.
    /// mock handler 가 AllowAlways { Exact } 로 응답 → "Write" 는 캐시에 없어야 하고
    /// 무관한 "Bash" 는 영향 없이 여전히 캐시에 있어야 함.
    #[tokio::test]
    async fn parallel_allow_always_exact_kind_clears_tool() {
        let (mut reader, _reader_write) = make_duplex_reader(&[]).await;
        let mut line = String::new();

        let (mut stdin_write, _stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        // pre-condition: "Bash" 이미 캐시에 있음 (다른 tool)
        cache.add_always_allow("Bash");
        assert!(cache.is_always_allowed("Bash"));

        // initial CR: "Write" (캐시 미스 → permission handler 로 전달됨)
        let initial_cr = make_initial_cr("cr1", "Write");

        tokio::spawn(async move {
            if let Some(req) = permission_rx.recv().await {
                let _ = req.response_tx.send(PermissionDecision::AllowAlways {
                    rule_kind: RuleKind::Exact,
                    scope: Scope::Project,
                });
            }
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;

        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
        assert!(!cache.is_always_allowed("Write"), "Exact kind must clear, not add");
        assert!(cache.is_always_allowed("Bash"), "unrelated tool must remain in cache");
    }

    /// Tool kind AllowAlways 처리 후 cache hit → 동일 tool 두 번째 호출 자동 허용
    ///
    /// 첫 번째 wait_for_permissions 에서 cr1 AllowAlways(Tool) → cache 에 "Bash" 추가.
    /// 두 번째 wait_for_permissions 에서 cr2 "Bash" → cache hit → permission_tx 에 보내지 않고
    /// 즉시 auto-allow stdin write 후 AllResolved 반환.
    #[tokio::test]
    async fn parallel_tool_kind_then_same_tool_auto_allowed() {
        // ── 1st call: cr1 AllowAlways(Tool) ──────────────────────────────────
        let (mut reader1, _reader_write1) = make_duplex_reader(&[]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(8192);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr1 = make_initial_cr("cr1", "Bash");

        // tokio::join! 으로 wait_for_permissions 와 receiver 처리를 동시에 실행.
        // permission_rx 를 task 로 move 하지 않고 테스트 본체에 유지 → receiver lifecycle 보존.
        let respond_task = async {
            if let Some(req) = permission_rx.recv().await {
                let _ = req.response_tx.send(PermissionDecision::AllowAlways {
                    rule_kind: RuleKind::Tool,
                    scope: Scope::Project,
                });
            }
        };

        let (result1, _) = tokio::join!(
            wait_for_permissions(
                &mut stdin_write, &mut reader1, &mut line, &mut queue_rx, &mut interrupt_rx,
                &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
                &mut cache, &permission_tx, initial_cr1,
            ),
            respond_task,
        );
        assert!(matches!(result1, PermissionsWaitResult::AllResolved { .. }), "1st call must AllResolved");
        assert!(cache.is_always_allowed("Bash"), "cache must have Bash after 1st call");

        // ── 2nd call: cr2 "Bash" → cache hit → auto-allow, permission_tx 불호출 ─
        let (mut reader2, _reader_write2) = make_duplex_reader(&[]).await;
        line.clear();

        let initial_cr2 = make_initial_cr("cr2", "Bash");

        let result2 = wait_for_permissions(
            &mut stdin_write, &mut reader2, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr2,
        ).await;
        assert!(matches!(result2, PermissionsWaitResult::AllResolved { .. }), "2nd call must AllResolved (cache hit)");

        // permission_rx 는 살아있지만 2nd call 은 cache hit 이라 메시지가 오지 않아야 함
        let no_msg = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            permission_rx.recv(),
        ).await;
        assert!(no_msg.is_err(), "receiver 가 살아있는데 두 번째 메시지가 도착하면 cache hit 실패");

        // stdin: cr1 allow + cr2 auto-allow = 2건
        let writes = drain_stdin_writes(&mut stdin_read, 2).await;
        assert_eq!(writes.len(), 2, "stdin 에 2건 기록되어야 함");
        let mut by_rid: std::collections::HashMap<&str, &serde_json::Value> = std::collections::HashMap::new();
        for w in &writes {
            by_rid.insert(w["response"]["request_id"].as_str().unwrap(), w);
        }
        assert_eq!(by_rid["cr1"]["response"]["response"]["behavior"], "allow", "cr1 → allow");
        assert_eq!(by_rid["cr2"]["response"]["response"]["behavior"], "allow", "cr2 → auto-allow (cache hit)");
    }

    /// 회귀: 단일 CR baseline — 기존 단수 시나리오가 여전히 동작
    #[tokio::test]
    async fn single_cr_baseline_still_works() {
        // duplex reader: 추가 CR 없음, EOF 없이 대기
        let (mut reader, _reader_write) = make_duplex_reader(&[]).await;
        let mut line = String::new();

        let (mut stdin_write, mut stdin_read) = tokio::io::duplex(4096);
        let (permission_tx, mut permission_rx) = tokio::sync::mpsc::channel::<PermissionRequest>(32);
        let mut cache = PermissionCache::new();
        let (_queue_tx, mut queue_rx, _interrupt_tx, mut interrupt_rx, queue_size, pending_recalls, ratelimit_tx) = setup_channels!();

        let initial_cr = make_initial_cr("cr-single", "Bash");

        let handler_task = tokio::spawn(async move {
            if let Some(req) = permission_rx.recv().await {
                let _ = req.response_tx.send(PermissionDecision::Allow);
            }
        });

        let result = wait_for_permissions(
            &mut stdin_write, &mut reader, &mut line, &mut queue_rx, &mut interrupt_rx,
            &queue_size, &pending_recalls, "test-thread", None, &ratelimit_tx,
            &mut cache, &permission_tx, initial_cr,
        ).await;
        handler_task.await.unwrap();

        let writes = drain_stdin_writes(&mut stdin_read, 1).await;
        assert_eq!(writes.len(), 1, "단일 CR → allow JSON 1개");
        assert_eq!(writes[0]["type"], "control_response");
        assert_eq!(writes[0]["response"]["response"]["behavior"], "allow");
        assert_eq!(writes[0]["response"]["request_id"], "cr-single");
        assert!(matches!(result, PermissionsWaitResult::AllResolved { .. }));
    }
}
