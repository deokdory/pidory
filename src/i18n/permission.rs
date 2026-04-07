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
}
