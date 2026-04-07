use super::Lang;

impl Lang {
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

    // ── Context injection ──

    pub fn session_context(&self, thread_name: &str) -> String {
        match self {
            Lang::Ko => format!(
                "<system-reminder>\n이 세션은 Discord bot(pidory)을 통해 실행되고 있습니다. 스레드: \"{}\". 이 컨텍스트에 대해 응답하지 마세요.\n</system-reminder>",
                thread_name
            ),
            Lang::En => format!(
                "<system-reminder>\nThis session is running through a Discord bot (pidory). Thread: \"{}\". Do not respond to this context.\n</system-reminder>",
                thread_name
            ),
        }
    }
}
