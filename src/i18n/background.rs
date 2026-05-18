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

    pub fn session_context(&self, thread_name: &str, thread_id: &str) -> String {
        match self {
            Lang::Ko => format!(
                "<system-reminder>\n이 세션은 Discord bot(pidory)을 통해 실행되고 있습니다. 스레드: \"{}\" (thread_id: {}). 이 컨텍스트에 대해 응답하지 마세요.\n파일을 Discord에 첨부하려면 /pidory-toss 스킬을 사용하세요.\n턴 마지막에 다음 단계 스킬을 제안할 때, `/skill-name` 형식으로 작성하세요. Discord에서 클릭 가능한 버튼으로 자동 변환됩니다.\n다른 사용자를 호명할 때는 `@username` 형식으로 작성하면 pidory가 자동으로 Discord 멘션으로 변환해줘요 (예: `@deokdory`). `@everyone`, `@here`는 사용하지 마세요.\n</system-reminder>",
                thread_name, thread_id
            ),
            Lang::En => format!(
                "<system-reminder>\nThis session is running through a Discord bot (pidory). Thread: \"{}\" (thread_id: {}). Do not respond to this context.\nTo attach files to Discord, use the /pidory-toss skill.\nWhen suggesting next steps at the end of a turn, use `/skill-name` format. They will be automatically converted to clickable Discord buttons.\nTo mention another user, write `@username` (e.g., `@deokdory`) — pidory will convert it to a Discord mention. Do not use `@everyone` or `@here`.\n</system-reminder>",
                thread_name, thread_id
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Baseline snapshots (T4) ──
    // 원문(제거 전) 에 T1이 thread_id 표기를 추가한 최종 Ko/En 기대 문자열.
    // thread_name = "my-thread", thread_id = "1234567890" 기준.

    const KO_BASELINE: &str = concat!(
        "<system-reminder>\n",
        "이 세션은 Discord bot(pidory)을 통해 실행되고 있습니다. 스레드: \"my-thread\" (thread_id: 1234567890). 이 컨텍스트에 대해 응답하지 마세요.\n",
        "파일을 Discord에 첨부하려면 /pidory-toss 스킬을 사용하세요.\n",
        "턴 마지막에 다음 단계 스킬을 제안할 때, `/skill-name` 형식으로 작성하세요. Discord에서 클릭 가능한 버튼으로 자동 변환됩니다.\n",
        "다른 사용자를 호명할 때는 `@username` 형식으로 작성하면 pidory가 자동으로 Discord 멘션으로 변환해줘요 (예: `@deokdory`). `@everyone`, `@here`는 사용하지 마세요.\n",
        "</system-reminder>"
    );

    const EN_BASELINE: &str = concat!(
        "<system-reminder>\n",
        "This session is running through a Discord bot (pidory). Thread: \"my-thread\" (thread_id: 1234567890). Do not respond to this context.\n",
        "To attach files to Discord, use the /pidory-toss skill.\n",
        "When suggesting next steps at the end of a turn, use `/skill-name` format. They will be automatically converted to clickable Discord buttons.\n",
        "To mention another user, write `@username` (e.g., `@deokdory`) — pidory will convert it to a Discord mention. Do not use `@everyone` or `@here`.\n",
        "</system-reminder>"
    );

    // (a) Ko payload 동등성 — 제거 전 원문 + thread_id 표기
    #[test]
    fn session_context_ko_exact_payload() {
        let result = Lang::Ko.session_context("my-thread", "1234567890");
        assert_eq!(result, KO_BASELINE);
    }

    // (b) En payload 동등성 — 제거 전 원문 + thread_id 표기
    #[test]
    fn session_context_en_exact_payload() {
        let result = Lang::En.session_context("my-thread", "1234567890");
        assert_eq!(result, EN_BASELINE);
    }

    // (c) thread_name 정확 삽입 — 일반값 / 특수문자(백틱·따옴표 포함)
    #[test]
    fn session_context_thread_name_inserted() {
        let ko = Lang::Ko.session_context("feature-branch", "0000000001");
        assert!(ko.contains("\"feature-branch\""));

        let en = Lang::En.session_context("feature-branch", "0000000001");
        assert!(en.contains("\"feature-branch\""));
    }

    #[test]
    fn session_context_thread_name_special_chars() {
        // thread_name 에 백틱·작은따옴표 포함 → 그대로 삽입돼야 함
        let special = "`fix: it's broken`";
        let ko = Lang::Ko.session_context(special, "9999999999");
        assert!(ko.contains(special));

        let en = Lang::En.session_context(special, "9999999999");
        assert!(en.contains(special));
    }

    // (d) thread_id 정확 삽입
    #[test]
    fn session_context_thread_id_inserted() {
        let ko = Lang::Ko.session_context("some-thread", "1122334455");
        assert!(ko.contains("thread_id: 1122334455"));

        let en = Lang::En.session_context("some-thread", "1122334455");
        assert!(en.contains("thread_id: 1122334455"));
    }

    // Ko / En payload 는 서로 달라야 함 (smoke)
    #[test]
    fn session_context_ko_en_differ() {
        let ko = Lang::Ko.session_context("my-thread", "1234567890");
        let en = Lang::En.session_context("my-thread", "1234567890");
        assert_ne!(ko, en);
    }
}
