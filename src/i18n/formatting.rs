use super::Lang;

impl Lang {
    // ── Timeout messages ──

    pub fn soft_timeout_nudge(&self) -> &'static str {
        match self {
            Lang::Ko => "장시간 무응답 — 확인 메시지를 전송했습니다",
            Lang::En => "No response for a while — sent a check message",
        }
    }

    pub fn hard_timeout_kill(&self) -> &'static str {
        match self {
            Lang::Ko => "응답 시간 초과로 턴을 종료합니다. 다시 시도해 주세요.",
            Lang::En => "Turn timed out. Please try again.",
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

    // ── Question (AskUserQuestion) ──

    pub fn question_select_placeholder(&self) -> &'static str {
        match self {
            Lang::Ko => "답변을 선택하세요",
            Lang::En => "Select an answer",
        }
    }

    pub fn question_write_answer(&self) -> &'static str {
        match self {
            Lang::Ko => "직접 입력",
            Lang::En => "Write answer",
        }
    }

    pub fn question_modal_title(&self) -> &'static str {
        match self {
            Lang::Ko => "답변 입력",
            Lang::En => "Enter answer",
        }
    }

    pub fn question_modal_label(&self) -> &'static str {
        match self {
            Lang::Ko => "답변",
            Lang::En => "Answer",
        }
    }

    pub fn question_modal_placeholder(&self) -> &'static str {
        match self {
            Lang::Ko => "답변을 입력하세요...",
            Lang::En => "Type your answer...",
        }
    }

    pub fn question_answered(&self) -> &'static str {
        match self {
            Lang::Ko => "답변:",
            Lang::En => "Answered:",
        }
    }

    pub fn question_cancel(&self) -> &'static str {
        match self {
            Lang::Ko => "취소",
            Lang::En => "Cancel",
        }
    }

    pub fn question_cancel_confirm_prompt(&self) -> &'static str {
        match self {
            Lang::Ko => "답변 없이 취소하시겠습니까? Claude는 응답 없이 다음 단계로 진행해요.",
            Lang::En => "Cancel without answering? Claude will proceed to the next step without a response.",
        }
    }

    pub fn question_cancel_confirm_yes(&self) -> &'static str {
        match self {
            Lang::Ko => "네, 취소함",
            Lang::En => "Yes, cancel",
        }
    }

    pub fn question_cancel_confirm_no(&self) -> &'static str {
        match self {
            Lang::Ko => "아니요",
            Lang::En => "No",
        }
    }

    pub fn question_canceled_label(&self) -> &'static str {
        match self {
            Lang::Ko => "✋ 취소됨",
            Lang::En => "✋ Canceled",
        }
    }
}
