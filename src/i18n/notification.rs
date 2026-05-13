use super::Lang;

impl Lang {
    // ── Rate limits ──

    pub fn rate_limit_reached(&self) -> &'static str {
        match self {
            Lang::Ko => "⚠️ Rate limit에 도달했어요",
            Lang::En => "⚠️ Rate limit reached",
        }
    }

    pub fn rate_limit_alert(&self, pct: u8, threshold: u8, remaining: &str) -> String {
        match self {
            Lang::Ko => format!(
                "⚠️ Rate limit 알림 — 5h 사용량이 {}%예요 (임계값 {}%){}",
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
            Lang::Ko => format!(" — {}시간{}분 뒤에 리셋돼요", h, m),
            Lang::En => format!(" — resets in {}h{}m", h, m),
        }
    }

    // ── Release notifications ──

    pub fn release_notify_title(&self, tag: &str) -> String {
        match self {
            Lang::Ko => format!("🚀 pidory {} Released", tag),
            Lang::En => format!("🚀 pidory {} Released", tag),
        }
    }

    pub fn release_body_truncated(&self, url: &str) -> String {
        match self {
            Lang::Ko => format!("… [전체 릴리즈 노트]({})", url),
            Lang::En => format!("… [Full release notes]({})", url),
        }
    }

    pub fn release_no_body(&self) -> &'static str {
        match self {
            Lang::Ko => "릴리즈 노트가 없어요.",
            Lang::En => "No release notes available.",
        }
    }

    // ── Formatting ──

    pub fn response_continues(&self) -> &'static str {
        match self {
            Lang::Ko => "*(응답이 첨부 파일에서 이어져요)*",
            Lang::En => "*(response continues in attachment)*",
        }
    }

    pub fn truncated_suffix(&self) -> &'static str {
        match self {
            Lang::Ko => "...(잘림)",
            Lang::En => "...(truncated)",
        }
    }

    // ── File attachment ──

    pub fn file_attached(&self, filename: &str, size_str: &str) -> String {
        match self {
            Lang::Ko => format!("📎 **{}** ({})", filename, size_str),
            Lang::En => format!("📎 **{}** ({})", filename, size_str),
        }
    }

    pub fn file_too_large(&self, filename: &str, size_mb: f64) -> String {
        match self {
            Lang::Ko => format!(
                "❌ 파일 크기가 너무 커요: **{}** ({:.1} MB) — Discord 한도 25 MB",
                filename, size_mb
            ),
            Lang::En => format!(
                "❌ File too large: **{}** ({:.1} MB) — Discord limit 25 MB",
                filename, size_mb
            ),
        }
    }

    pub fn file_not_found(&self, path: &str) -> String {
        match self {
            Lang::Ko => format!("❌ 파일을 찾을 수 없어요: `{}`", path),
            Lang::En => format!("❌ File not found: `{}`", path),
        }
    }

    pub fn file_permission_denied(&self, path: &str) -> String {
        match self {
            Lang::Ko => format!("❌ 파일 읽기 권한이 없어요: `{}`", path),
            Lang::En => format!("❌ Permission denied: `{}`", path),
        }
    }

    pub fn file_attach_error(&self, path: &str, error: &str) -> String {
        match self {
            Lang::Ko => format!("❌ 파일을 보내지 못했어요: `{}` — {}", path, error),
            Lang::En => format!("❌ File transfer failed: `{}` — {}", path, error),
        }
    }
}
