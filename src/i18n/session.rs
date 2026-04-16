use super::Lang;

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

    // ── Recall ──

    pub fn recall_success(&self) -> &'static str {
        match self {
            Lang::Ko => "회수 완료",
            Lang::En => "Message recalled",
        }
    }

    pub fn recall_already_sent(&self) -> &'static str {
        match self {
            Lang::Ko => "이미 전달된 메시지입니다",
            Lang::En => "Message already sent",
        }
    }

    pub fn recall_no_session(&self) -> &'static str {
        match self {
            Lang::Ko => "활성 세션이 없습니다",
            Lang::En => "No active session",
        }
    }

    pub fn session_reset(&self) -> &'static str {
        match self {
            Lang::Ko => "세션을 리셋했습니다. 다음 메시지부터 새 세션이 시작됩니다.",
            Lang::En => "Session reset. A new session will start with your next message.",
        }
    }

    pub fn session_cleared_by(&self, mention: &str) -> String {
        match self {
            Lang::Ko => format!("{}님에 의해서 세션이 클리어 되었습니다", mention),
            Lang::En => format!("Session cleared by {}", mention),
        }
    }

    pub fn session_reset_confirm(&self) -> &'static str {
        match self {
            Lang::Ko => "세션이 작업 중입니다. 리셋하시겠습니까?",
            Lang::En => "Session is busy. Reset it?",
        }
    }

    pub fn session_reset_cancelled(&self) -> &'static str {
        match self {
            Lang::Ko => "리셋이 취소되었습니다.",
            Lang::En => "Reset cancelled.",
        }
    }

    pub fn session_reset_expired(&self) -> &'static str {
        match self {
            Lang::Ko => "리셋 확인이 만료되었습니다.",
            Lang::En => "Reset confirmation expired.",
        }
    }

    pub fn session_reset_interrupt_failed(&self, e: &impl std::fmt::Display) -> String {
        match self {
            Lang::Ko => format!("세션 중단 실패: {}", e),
            Lang::En => format!("Failed to interrupt session: {}", e),
        }
    }
}
