use super::Lang;

impl Lang {
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
}
