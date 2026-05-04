use crate::handler::formatter::inline_code;

use super::Lang;

impl Lang {
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
            Lang::Ko => format!("{} — 허용됨", inline_code(tool)),
            Lang::En => format!("{} — Allowed", inline_code(tool)),
        }
    }

    pub fn perm_always_allowed(&self, tool: &str) -> String {
        match self {
            Lang::Ko => format!("{} — 항상 허용됨", inline_code(tool)),
            Lang::En => format!("{} — Always Allowed", inline_code(tool)),
        }
    }

    pub fn perm_denied(&self, tool: &str) -> String {
        match self {
            Lang::Ko => format!("{} — 거부됨", inline_code(tool)),
            Lang::En => format!("{} — Denied", inline_code(tool)),
        }
    }

    pub fn btn_once(&self) -> &'static str {
        match self {
            Lang::Ko => "한 번만",
            Lang::En => "Allow Once",
        }
    }

    pub fn btn_always_exact(&self) -> &'static str {
        match self {
            Lang::Ko => "이 명령만",
            Lang::En => "This command only",
        }
    }

    pub fn btn_always_prefix(&self) -> &'static str {
        match self {
            Lang::Ko => "같은 prefix",
            Lang::En => "Same prefix",
        }
    }

    pub fn btn_always_domain(&self) -> &'static str {
        match self {
            Lang::Ko => "같은 도메인",
            Lang::En => "Same domain",
        }
    }

    pub fn btn_always_tool(&self) -> &'static str {
        match self {
            Lang::Ko => "도구 전체",
            Lang::En => "Entire tool",
        }
    }

    pub fn btn_scope_global_off(&self) -> &'static str {
        "🌐 global ⊘"
    }

    pub fn btn_scope_global_on(&self) -> &'static str {
        "🌐 global ✓"
    }

    pub fn msg_save_success(&self, rule: &str) -> String {
        match self {
            Lang::Ko => format!("✅ rule 저장됨: {rule}"),
            Lang::En => format!("✅ Rule saved: {rule}"),
        }
    }

    pub fn msg_save_failed_lock_timeout(&self) -> &'static str {
        match self {
            Lang::Ko => "⏱ 권한 저장 실패 (lock timeout) — 다시 시도하세요",
            Lang::En => "⏱ Permission save failed (lock timeout) — please retry",
        }
    }

    pub fn msg_conflict_title(&self) -> &'static str {
        match self {
            Lang::Ko => "권한 충돌 알림",
            Lang::En => "Permission conflict",
        }
    }
}
