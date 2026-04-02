use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Ko,
    En,
}

impl Lang {
    // ── Session lifecycle ──

    pub fn session_evicted(&self) -> &'static str {
        match self {
            Lang::Ko => "세션이 새 요청을 위해 정리되었습니다. 메시지를 보내면 자동으로 재개됩니다.",
            Lang::En => "Session evicted for new request. Send a message to resume.",
        }
    }

    pub fn session_idle_cleaned(&self) -> &'static str {
        match self {
            Lang::Ko => "세션이 비활성으로 정리되었습니다. 메시지를 보내면 자동으로 재개됩니다.",
            Lang::En => "Session cleaned due to inactivity. Send a message to resume.",
        }
    }

    pub fn session_create_failed(&self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::Ko => format!("세션 생성 실패: {}", err),
            Lang::En => format!("Session creation failed: {}", err),
        }
    }

    pub fn message_send_failed(&self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::Ko => format!("메시지 전송 실패: {}", err),
            Lang::En => format!("Failed to send message: {}", err),
        }
    }

    pub fn queue_full(&self) -> &'static str {
        match self {
            Lang::Ko => "대기열이 가득 찼습니다",
            Lang::En => "Queue is full",
        }
    }

    pub fn error_with(&self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::Ko => format!("오류: {}", err),
            Lang::En => format!("Error: {}", err),
        }
    }

    // ── Completion & errors ──

    pub fn process_abnormal_exit(&self) -> &'static str {
        match self {
            Lang::Ko => "프로세스가 비정상 종료되었습니다",
            Lang::En => "Process terminated abnormally",
        }
    }

    pub fn error_occurred(&self) -> &'static str {
        match self {
            Lang::Ko => "에러 발생",
            Lang::En => "Error occurred",
        }
    }

    // ── Permissions ──

    pub fn permission_request_label(&self) -> &'static str {
        match self {
            Lang::Ko => "실행 허가 요청",
            Lang::En => "Permission request",
        }
    }

    pub fn no_permission(&self) -> &'static str {
        match self {
            Lang::Ko => "권한이 없습니다",
            Lang::En => "Permission denied",
        }
    }

    pub fn btn_allow(&self) -> &'static str {
        match self {
            Lang::Ko => "허용",
            Lang::En => "Allow",
        }
    }

    pub fn btn_always_allow(&self) -> &'static str {
        match self {
            Lang::Ko => "항상 허용",
            Lang::En => "Always Allow",
        }
    }

    pub fn btn_deny(&self) -> &'static str {
        match self {
            Lang::Ko => "거부",
            Lang::En => "Deny",
        }
    }

    pub fn perm_allowed(&self, tool: &str) -> String {
        match self {
            Lang::Ko => format!("{} — 허용됨", tool),
            Lang::En => format!("{} — Allowed", tool),
        }
    }

    pub fn perm_always_allowed(&self, tool: &str) -> String {
        match self {
            Lang::Ko => format!("{} — 항상 허용됨", tool),
            Lang::En => format!("{} — Always Allowed", tool),
        }
    }

    pub fn perm_denied(&self, tool: &str) -> String {
        match self {
            Lang::Ko => format!("{} — 거부됨", tool),
            Lang::En => format!("{} — Denied", tool),
        }
    }

    // ── Status display ──

    pub fn working(&self) -> &'static str {
        match self {
            Lang::Ko => "작업 중...",
            Lang::En => "Working...",
        }
    }

    pub fn status_error(&self, err: &str) -> String {
        match self {
            Lang::Ko => format!("오류 — {}", err),
            Lang::En => format!("Error — {}", err),
        }
    }

    pub fn more_items(&self, count: usize) -> String {
        match self {
            Lang::Ko => format!("... +{} 더보기", count),
            Lang::En => format!("... +{} more", count),
        }
    }

    // ── Commands: register ──

    pub fn path_not_exist(&self, path: &str) -> String {
        match self {
            Lang::Ko => format!("경로가 존재하지 않습니다: `{}`", path),
            Lang::En => format!("Path does not exist: `{}`", path),
        }
    }

    pub fn already_registered(&self, path: &str) -> String {
        match self {
            Lang::Ko => format!(
                "이 채널은 이미 `{}`에 등록되어 있습니다. `/unregister`를 먼저 실행하세요.",
                path
            ),
            Lang::En => format!(
                "This channel is already registered to `{}`. Use `/unregister` first.",
                path
            ),
        }
    }

    pub fn registered(&self, path: &str) -> String {
        match self {
            Lang::Ko => format!("`{}`이(가) 이 채널에 등록되었습니다", path),
            Lang::En => format!("Registered `{}` to this channel", path),
        }
    }

    pub fn not_registered(&self) -> &'static str {
        match self {
            Lang::Ko => "이 채널에 등록된 프로젝트가 없습니다",
            Lang::En => "No project registered to this channel",
        }
    }

    pub fn unregistered(&self) -> &'static str {
        match self {
            Lang::Ko => "이 채널에서 프로젝트 등록이 해제되었습니다",
            Lang::En => "Unregistered project from this channel",
        }
    }

    // ── Commands: session ──

    pub fn no_active_sessions_short(&self) -> &'static str {
        match self {
            Lang::Ko => "활성 세션 없음",
            Lang::En => "No active sessions",
        }
    }

    pub fn active_sessions_header(&self, count: usize, max: usize) -> String {
        match self {
            Lang::Ko => format!("📊 활성 세션 ({}/{})", count, max),
            Lang::En => format!("📊 Active Sessions ({}/{})", count, max),
        }
    }

    pub fn active_sessions_list_header(&self) -> &'static str {
        match self {
            Lang::Ko => "📋 활성 세션:",
            Lang::En => "📋 Active Sessions:",
        }
    }

    pub fn no_session_in_thread(&self) -> &'static str {
        match self {
            Lang::Ko => "이 스레드에 활성 세션이 없습니다",
            Lang::En => "No active session in this thread",
        }
    }

    pub fn interrupted(&self) -> &'static str {
        match self {
            Lang::Ko => "중단됨",
            Lang::En => "Interrupted",
        }
    }

    pub fn interrupt_failed(&self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::Ko => format!("중단 실패: {}", err),
            Lang::En => format!("Interrupt failed: {}", err),
        }
    }

    pub fn not_in_thread(&self) -> &'static str {
        match self {
            Lang::Ko => "스레드가 아닙니다. 스레드 ID를 직접 입력하세요.",
            Lang::En => "Not in a thread. Provide a thread ID explicitly.",
        }
    }

    pub fn no_session_found(&self, tid: &str) -> String {
        match self {
            Lang::Ko => format!("스레드 `{}`에 세션이 없습니다", tid),
            Lang::En => format!("No session found for thread `{}`", tid),
        }
    }

    pub fn session_deleted(&self) -> &'static str {
        match self {
            Lang::Ko => "세션이 삭제되었습니다",
            Lang::En => "Session deleted",
        }
    }

    pub fn session_status_display(
        &self,
        thread_id: &str,
        status: &str,
        session_id: &str,
        last_active: &str,
    ) -> String {
        match self {
            Lang::Ko => format!(
                "📊 세션 상태\n스레드: <#{}>\n상태: {}\n세션 ID: {}\n마지막 활성: {}",
                thread_id, status, session_id, last_active
            ),
            Lang::En => format!(
                "📊 Session Status\nThread: <#{}>\nStatus: {}\nSession ID: {}\nLast Active: {}",
                thread_id, status, session_id, last_active
            ),
        }
    }

    pub fn running_status(&self) -> &'static str {
        match self {
            Lang::Ko => "🔄 실행 중",
            Lang::En => "🔄 running",
        }
    }

    pub fn bg_tasks_suffix(&self) -> &'static str {
        match self {
            Lang::Ko => " — 백그라운드 작업",
            Lang::En => " — bg tasks",
        }
    }

    pub fn none_placeholder(&self) -> &'static str {
        match self {
            Lang::Ko => "(없음)",
            Lang::En => "(none)",
        }
    }

    pub fn never_placeholder(&self) -> &'static str {
        match self {
            Lang::Ko => "(없음)",
            Lang::En => "(never)",
        }
    }

    pub fn session_list_row(
        &self,
        thread_mention: &str,
        status: &str,
        session_short: &str,
        since: &str,
    ) -> String {
        match self {
            Lang::Ko => format!(
                "• {} — 상태: {}{}{}",
                thread_mention, status, session_short, since
            ),
            Lang::En => format!(
                "• {} — status: {}{}{}",
                thread_mention, status, session_short, since
            ),
        }
    }

    pub fn session_list_since(&self, relative: &str) -> String {
        match self {
            Lang::Ko => format!(" — 시작: {}", relative),
            Lang::En => format!(" — since: {}", relative),
        }
    }

    pub fn session_list_id(&self, short_id: &str) -> String {
        match self {
            Lang::Ko => format!(" — 세션: {}…", short_id),
            Lang::En => format!(" — session: {}…", short_id),
        }
    }

    // ── Background task messages ──

    pub fn bg_permission_denied(&self, tool_name: &str) -> String {
        match self {
            Lang::Ko => format!("-# ⚠️ [백그라운드] 권한 거부: {} (캐시에 없음)", tool_name),
            Lang::En => format!("-# ⚠️ [Background] Permission denied: {} (not in cache)", tool_name),
        }
    }

    pub fn bg_permission_deny_reason(&self) -> &'static str {
        match self {
            Lang::Ko => "백그라운드: 권한 캐시에 없음",
            Lang::En => "Background: permission not cached",
        }
    }

    pub fn bg_notification(&self, text: &str) -> String {
        match self {
            Lang::Ko => format!("-# 🔔 [백그라운드]\n{}", text),
            Lang::En => format!("-# 🔔 [Background]\n{}", text),
        }
    }

    pub fn bg_task_started(&self, description: &str) -> String {
        match self {
            Lang::Ko => format!("-# 🔔 백그라운드 작업 시작: {}", description),
            Lang::En => format!("-# 🔔 Background task started: {}", description),
        }
    }

    // ── Rate limits ──

    pub fn rate_limit_reached(&self) -> &'static str {
        match self {
            Lang::Ko => "⚠️ Rate limit 도달",
            Lang::En => "⚠️ Rate limit reached",
        }
    }

    pub fn rate_limit_alert(&self, pct: u8, threshold: u8, remaining: &str) -> String {
        match self {
            Lang::Ko => format!(
                "⚠️ Rate limit 알림: 5h 사용량 {}% (임계값: {}%){}",
                pct, threshold, remaining
            ),
            Lang::En => format!(
                "⚠️ Rate limit alert: 5h usage at {}% (threshold: {}%){}",
                pct, threshold, remaining
            ),
        }
    }

    pub fn resets_in(&self, h: u64, m: u64) -> String {
        match self {
            Lang::Ko => format!(" — {}시간{}분 후 리셋", h, m),
            Lang::En => format!(" — resets in {}h{}m", h, m),
        }
    }

    // ── Formatting ──

    pub fn response_continues(&self) -> &'static str {
        match self {
            Lang::Ko => "*(응답이 첨부 파일에 계속됩니다)*",
            Lang::En => "*(response continues in attachment)*",
        }
    }

    pub fn truncated_suffix(&self) -> &'static str {
        match self {
            Lang::Ko => "...(잘림)",
            Lang::En => "...(truncated)",
        }
    }

    // ── Time formatting ──

    pub fn format_relative_time(&self, diff_secs: u64) -> String {
        match self {
            Lang::Ko => {
                if diff_secs < 60 {
                    format!("{}초 전", diff_secs)
                } else if diff_secs < 3600 {
                    format!("{}분 전", diff_secs / 60)
                } else if diff_secs < 86400 {
                    format!("{}시간 전", diff_secs / 3600)
                } else {
                    format!("{}일 전", diff_secs / 86400)
                }
            }
            Lang::En => {
                if diff_secs < 60 {
                    format!("{}s ago", diff_secs)
                } else if diff_secs < 3600 {
                    format!("{}m ago", diff_secs / 60)
                } else if diff_secs < 86400 {
                    format!("{}h ago", diff_secs / 3600)
                } else {
                    format!("{}d ago", diff_secs / 86400)
                }
            }
        }
    }

    pub fn format_idle(&self, secs: u64) -> String {
        match self {
            Lang::Ko => {
                if secs < 60 {
                    format!("유휴 {}초", secs)
                } else if secs < 3600 {
                    format!("유휴 {}분", secs / 60)
                } else {
                    format!("유휴 {}시간{}분", secs / 3600, (secs % 3600) / 60)
                }
            }
            Lang::En => {
                if secs < 60 {
                    format!("idle {}s", secs)
                } else if secs < 3600 {
                    format!("idle {}m", secs / 60)
                } else {
                    format!("idle {}h{}m", secs / 3600, (secs % 3600) / 60)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_ko() {
        assert_eq!(Lang::default(), Lang::Ko);
    }

    #[test]
    fn deserialize_from_string() {
        let ko: Lang = serde_json::from_str(r#""ko""#).unwrap();
        assert_eq!(ko, Lang::Ko);
        let en: Lang = serde_json::from_str(r#""en""#).unwrap();
        assert_eq!(en, Lang::En);
    }

    #[test]
    fn session_evicted_both_langs() {
        assert!(Lang::Ko.session_evicted().contains("정리"));
        assert!(Lang::En.session_evicted().contains("evicted"));
    }

    #[test]
    fn format_relative_time_ko() {
        assert_eq!(Lang::Ko.format_relative_time(30), "30초 전");
        assert_eq!(Lang::Ko.format_relative_time(120), "2분 전");
        assert_eq!(Lang::Ko.format_relative_time(7200), "2시간 전");
        assert_eq!(Lang::Ko.format_relative_time(172800), "2일 전");
    }

    #[test]
    fn format_relative_time_en() {
        assert_eq!(Lang::En.format_relative_time(30), "30s ago");
        assert_eq!(Lang::En.format_relative_time(120), "2m ago");
        assert_eq!(Lang::En.format_relative_time(7200), "2h ago");
        assert_eq!(Lang::En.format_relative_time(172800), "2d ago");
    }

    #[test]
    fn format_idle_ko() {
        assert_eq!(Lang::Ko.format_idle(30), "유휴 30초");
        assert_eq!(Lang::Ko.format_idle(150), "유휴 2분");
        assert_eq!(Lang::Ko.format_idle(7200), "유휴 2시간0분");
        assert_eq!(Lang::Ko.format_idle(5430), "유휴 1시간30분");
    }

    #[test]
    fn format_idle_en() {
        assert_eq!(Lang::En.format_idle(30), "idle 30s");
        assert_eq!(Lang::En.format_idle(150), "idle 2m");
        assert_eq!(Lang::En.format_idle(7200), "idle 2h0m");
        assert_eq!(Lang::En.format_idle(5430), "idle 1h30m");
    }

    #[test]
    fn permission_labels_differ() {
        assert_ne!(Lang::Ko.btn_allow(), Lang::En.btn_allow());
        assert_ne!(Lang::Ko.btn_deny(), Lang::En.btn_deny());
    }

    #[test]
    fn format_with_args() {
        let err = "timeout";
        assert!(Lang::Ko.session_create_failed(&err).contains("timeout"));
        assert!(Lang::En.session_create_failed(&err).contains("timeout"));
    }
}
