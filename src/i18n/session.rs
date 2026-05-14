use super::Lang;

impl Lang {
    // ── Session lifecycle ──

    pub fn session_evicted(&self) -> &'static str {
        match self {
            Lang::Ko => "세션이 새 요청을 위해 정리됐어요. 메시지를 보내면 자동으로 재개돼요.",
            Lang::En => "Session evicted for new request. Send a message to resume.",
        }
    }

    pub fn session_idle_cleaned(&self) -> &'static str {
        match self {
            Lang::Ko => "세션이 비활성으로 정리됐어요. 메시지를 보내면 자동으로 재개돼요.",
            Lang::En => "Session cleaned due to inactivity. Send a message to resume.",
        }
    }

    pub fn session_create_failed(&self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::Ko => format!("세션을 만들지 못했어요: {}", err),
            Lang::En => format!("Session creation failed: {}", err),
        }
    }

    pub fn message_send_failed(&self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::Ko => format!("메시지를 보내지 못했어요: {}", err),
            Lang::En => format!("Failed to send message: {}", err),
        }
    }

    pub fn queue_full(&self) -> &'static str {
        match self {
            Lang::Ko => "대기열이 가득 찼어요",
            Lang::En => "Queue is full",
        }
    }

    pub fn error_with(&self, err: &impl std::fmt::Display) -> String {
        match self {
            Lang::Ko => format!("오류가 발생했습니다: {}", err),
            Lang::En => format!("Error: {}", err),
        }
    }

    // ── Completion & errors ──

    pub fn process_abnormal_exit(&self) -> &'static str {
        match self {
            Lang::Ko => "프로세스가 비정상 종료됐습니다",
            Lang::En => "Process terminated abnormally",
        }
    }

    pub fn error_occurred(&self) -> &'static str {
        match self {
            Lang::Ko => "오류가 발생했습니다",
            Lang::En => "Error occurred",
        }
    }

    // ── Recall ──

    pub fn recall_success(&self) -> &'static str {
        match self {
            Lang::Ko => "메시지를 회수했어요",
            Lang::En => "Message recalled",
        }
    }

    pub fn recall_already_sent(&self) -> &'static str {
        match self {
            Lang::Ko => "이미 전달된 메시지예요",
            Lang::En => "Message already sent",
        }
    }

    pub fn recall_no_session(&self) -> &'static str {
        match self {
            Lang::Ko => "활성 세션이 없어요",
            Lang::En => "No active session",
        }
    }

    pub fn session_reset(&self) -> &'static str {
        match self {
            Lang::Ko => "세션을 리셋했어요. 다음 메시지부터 새 세션이 시작돼요.",
            Lang::En => "Session reset. A new session will start with your next message.",
        }
    }

    pub fn session_cleared_by(&self, mention: &str) -> String {
        match self {
            Lang::Ko => format!("{}님이 세션을 클리어했어요", mention),
            Lang::En => format!("Session cleared by {}", mention),
        }
    }

    pub fn session_reset_confirm(&self) -> &'static str {
        match self {
            Lang::Ko => "세션이 작업 중이에요. 리셋할까요?",
            Lang::En => "Session is busy. Reset it?",
        }
    }

    pub fn session_reset_cancelled(&self) -> &'static str {
        match self {
            Lang::Ko => "리셋이 취소됐어요.",
            Lang::En => "Reset cancelled.",
        }
    }

    pub fn session_reset_expired(&self) -> &'static str {
        match self {
            Lang::Ko => "리셋 확인이 만료됐어요.",
            Lang::En => "Reset confirmation expired.",
        }
    }

    pub fn session_reset_interrupt_failed(&self, e: &impl std::fmt::Display) -> String {
        match self {
            Lang::Ko => format!("세션을 중단하지 못했어요: {}", e),
            Lang::En => format!("Failed to interrupt session: {}", e),
        }
    }

    // ── Reset UI ──

    pub fn btn_reset_confirm(&self) -> &'static str {
        match self {
            Lang::Ko => "✅ 네, 리셋",
            Lang::En => "✅ Yes, reset",
        }
    }

    pub fn btn_reset_cancel(&self) -> &'static str {
        match self {
            Lang::Ko => "❌ 아니요",
            Lang::En => "❌ No",
        }
    }

    pub fn reset_done(&self) -> &'static str {
        match self {
            Lang::Ko => "✅ 리셋됨",
            Lang::En => "✅ Reset complete",
        }
    }

    pub fn reset_cancelled_label(&self) -> &'static str {
        match self {
            Lang::Ko => "❌ 취소됨",
            Lang::En => "❌ Cancelled",
        }
    }

    pub fn reset_expired_label(&self) -> &'static str {
        match self {
            Lang::Ko => "⏰ 만료됨",
            Lang::En => "⏰ Expired",
        }
    }
}
