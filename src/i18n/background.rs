use super::Lang;

impl Lang {
    // ── Background task messages ──

    pub fn bg_permission_denied(&self, tool_name: &str) -> String {
        match self {
            Lang::Ko => format!("-# ⚠️ [백그라운드] 권한이 거부됐어요: {} (캐시에 없음)", tool_name),
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
            Lang::Ko => format!("-# 🔔 백그라운드 작업을 시작했어요: {}", description),
            Lang::En => format!("-# 🔔 Background task started: {}", description),
        }
    }

    // ── Context injection ──

    pub fn session_context(&self, thread_id: &str, channel_id: &str) -> String {
        match self {
            Lang::Ko => format!(
                "<system-reminder>\n이 세션은 Discord bot(pidory)을 통해 실행되고 있습니다. 스레드 ID: {}, 채널 ID: {}. 이 컨텍스트에 대해 응답하지 마세요.\n파일을 Discord에 첨부하려면 /pidory-toss 스킬을 사용하세요.\n턴 마지막에 다음 단계 스킬을 제안할 때, `/skill-name` 형식으로 작성하세요. Discord에서 클릭 가능한 버튼으로 자동 변환됩니다.\n다른 사용자를 호명할 때는 `@username` 형식으로 작성하면 pidory가 자동으로 Discord 멘션으로 변환해줘요 (예: `@deokdory`). `@everyone`, `@here`는 사용하지 마세요.\n</system-reminder>",
                thread_id, channel_id
            ),
            Lang::En => format!(
                "<system-reminder>\nThis session is running through a Discord bot (pidory). Thread ID: {}, Channel ID: {}. Do not respond to this context.\nTo attach files to Discord, use the /pidory-toss skill.\nWhen suggesting next steps at the end of a turn, use `/skill-name` format. They will be automatically converted to clickable Discord buttons.\nTo mention another user, write `@username` (e.g., `@deokdory`) — pidory will convert it to a Discord mention. Do not use `@everyone` or `@here`.\n</system-reminder>",
                thread_id, channel_id
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Baseline snapshots ──
    // thread_id = "1234567890", channel_id = "9876543210" 기준. thread_name 은 payload 에 없음.

    const KO_BASELINE: &str = concat!(
        "<system-reminder>\n",
        "이 세션은 Discord bot(pidory)을 통해 실행되고 있습니다. 스레드 ID: 1234567890, 채널 ID: 9876543210. 이 컨텍스트에 대해 응답하지 마세요.\n",
        "파일을 Discord에 첨부하려면 /pidory-toss 스킬을 사용하세요.\n",
        "턴 마지막에 다음 단계 스킬을 제안할 때, `/skill-name` 형식으로 작성하세요. Discord에서 클릭 가능한 버튼으로 자동 변환됩니다.\n",
        "다른 사용자를 호명할 때는 `@username` 형식으로 작성하면 pidory가 자동으로 Discord 멘션으로 변환해줘요 (예: `@deokdory`). `@everyone`, `@here`는 사용하지 마세요.\n",
        "</system-reminder>"
    );

    const EN_BASELINE: &str = concat!(
        "<system-reminder>\n",
        "This session is running through a Discord bot (pidory). Thread ID: 1234567890, Channel ID: 9876543210. Do not respond to this context.\n",
        "To attach files to Discord, use the /pidory-toss skill.\n",
        "When suggesting next steps at the end of a turn, use `/skill-name` format. They will be automatically converted to clickable Discord buttons.\n",
        "To mention another user, write `@username` (e.g., `@deokdory`) — pidory will convert it to a Discord mention. Do not use `@everyone` or `@here`.\n",
        "</system-reminder>"
    );

    // (a) Ko payload 동등성
    #[test]
    fn session_context_ko_exact_payload() {
        let result = Lang::Ko.session_context("1234567890", "9876543210");
        assert_eq!(result, KO_BASELINE);
    }

    // (b) En payload 동등성
    #[test]
    fn session_context_en_exact_payload() {
        let result = Lang::En.session_context("1234567890", "9876543210");
        assert_eq!(result, EN_BASELINE);
    }

    // (c) thread_id 정확 삽입
    #[test]
    fn session_context_thread_id_inserted() {
        let ko = Lang::Ko.session_context("1122334455", "9876543210");
        assert!(ko.contains("1122334455"));

        let en = Lang::En.session_context("1122334455", "9876543210");
        assert!(en.contains("1122334455"));
    }

    // (c2) channel_id 정확 삽입
    #[test]
    fn session_context_channel_id_inserted() {
        let ko = Lang::Ko.session_context("1234567890", "5544332211");
        assert!(ko.contains("5544332211"));

        let en = Lang::En.session_context("1234567890", "5544332211");
        assert!(en.contains("5544332211"));
    }

    // (d) Ko / En payload 는 서로 달라야 함 (smoke)
    #[test]
    fn session_context_ko_en_differ() {
        let ko = Lang::Ko.session_context("1234567890", "9876543210");
        let en = Lang::En.session_context("1234567890", "9876543210");
        assert_ne!(ko, en);
    }

    // (e) 인젝션 회귀: thread_id·channel_id 둘 다 숫자 snowflake — < > 개행 포함 불가
    // — payload 가 baseline 과 정확히 일치하므로 구조 파괴 불가
    #[test]
    fn session_context_injection_regression() {
        let ko = Lang::Ko.session_context("1234567890", "9876543210");
        assert_eq!(ko, KO_BASELINE);

        let en = Lang::En.session_context("1234567890", "9876543210");
        assert_eq!(en, EN_BASELINE);
    }
}
