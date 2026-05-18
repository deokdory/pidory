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

    /// `expose_user_id`: config.mention.expose_user_id. true 일 때 sender XML attribute 해석법 + raw <@id> 금지 안내를 포함한다.
    pub fn session_context(&self, thread_id: &str, channel_id: &str, expose_user_id: bool) -> String {
        let sender_hint = if expose_user_id {
            match self {
                Lang::Ko => concat!(
                    "사용자 메시지는 `<sender id=\"USER_ID\">표시명</sender>` 형식으로 전달됩니다. ",
                    "같은 id를 가진 메시지는 동일 사용자입니다. 사용자를 지칭할 때는 표시명을 사용하세요. ",
                    "`<@USER_ID>` 형태의 Discord mention을 직접 출력하지 마세요 — ",
                    "pidory가 표시명→id 변환을 전담합니다. LLM이 직접 생성하면 호칭-id 오결합이 발생합니다.\n",
                ),
                Lang::En => concat!(
                    "User messages are delivered as `<sender id=\"USER_ID\">display name</sender>`. ",
                    "Messages with the same id come from the same user. Use the display name when referring to a user. ",
                    "Do not emit raw `<@USER_ID>` Discord mentions — ",
                    "pidory exclusively handles display-name-to-id resolution. Direct LLM output causes name-id mismatch.\n",
                ),
            }
        } else {
            ""
        };
        match self {
            Lang::Ko => format!(
                "<system-reminder>\n이 세션은 Discord bot(pidory)을 통해 실행되고 있습니다. 스레드 ID: {thread_id}, 채널 ID: {channel_id}. 이 컨텍스트에 대해 응답하지 마세요.\n파일을 Discord에 첨부하려면 /pidory-toss 스킬을 사용하세요.\n턴 마지막에 다음 단계 스킬을 제안할 때, `/skill-name` 형식으로 작성하세요. Discord에서 클릭 가능한 버튼으로 자동 변환됩니다.\n다른 사용자를 호명할 때는 `@username` 형식으로 작성하면 pidory가 자동으로 Discord 멘션으로 변환해줘요 (예: `@deokdory`). `@everyone`, `@here`는 사용하지 마세요.\n멘션(ping)은 그 사람에게 직접 부탁/확인할 일이 있거나 그 사람이 꼭 봐야 하는 내용일 때만 하세요. 단순 언급이나 맥락상 제3자를 거론하는 경우엔 멘션하지 말고 이름만 쓰세요 (무분별한 ping 금지).\n{sender_hint}</system-reminder>",
            ),
            Lang::En => format!(
                "<system-reminder>\nThis session is running through a Discord bot (pidory). Thread ID: {thread_id}, Channel ID: {channel_id}. Do not respond to this context.\nTo attach files to Discord, use the /pidory-toss skill.\nWhen suggesting next steps at the end of a turn, use `/skill-name` format. They will be automatically converted to clickable Discord buttons.\nTo mention another user, write `@username` (e.g., `@deokdory`) — pidory will convert it to a Discord mention. Do not use `@everyone` or `@here`.\nOnly mention (ping) someone when you have a direct request/question for them or when they genuinely need to see the content. For mere references or when citing a third party in passing, do not mention — just use their name (no indiscriminate pinging).\n{sender_hint}</system-reminder>",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Baseline snapshots (expose_user_id=false) ──
    // thread_id = "1234567890", channel_id = "9876543210"

    const KO_BASELINE: &str = concat!(
        "<system-reminder>\n",
        "이 세션은 Discord bot(pidory)을 통해 실행되고 있습니다. 스레드 ID: 1234567890, 채널 ID: 9876543210. 이 컨텍스트에 대해 응답하지 마세요.\n",
        "파일을 Discord에 첨부하려면 /pidory-toss 스킬을 사용하세요.\n",
        "턴 마지막에 다음 단계 스킬을 제안할 때, `/skill-name` 형식으로 작성하세요. Discord에서 클릭 가능한 버튼으로 자동 변환됩니다.\n",
        "다른 사용자를 호명할 때는 `@username` 형식으로 작성하면 pidory가 자동으로 Discord 멘션으로 변환해줘요 (예: `@deokdory`). `@everyone`, `@here`는 사용하지 마세요.\n",
        "멘션(ping)은 그 사람에게 직접 부탁/확인할 일이 있거나 그 사람이 꼭 봐야 하는 내용일 때만 하세요. 단순 언급이나 맥락상 제3자를 거론하는 경우엔 멘션하지 말고 이름만 쓰세요 (무분별한 ping 금지).\n",
        "</system-reminder>"
    );

    const EN_BASELINE: &str = concat!(
        "<system-reminder>\n",
        "This session is running through a Discord bot (pidory). Thread ID: 1234567890, Channel ID: 9876543210. Do not respond to this context.\n",
        "To attach files to Discord, use the /pidory-toss skill.\n",
        "When suggesting next steps at the end of a turn, use `/skill-name` format. They will be automatically converted to clickable Discord buttons.\n",
        "To mention another user, write `@username` (e.g., `@deokdory`) — pidory will convert it to a Discord mention. Do not use `@everyone` or `@here`.\n",
        "Only mention (ping) someone when you have a direct request/question for them or when they genuinely need to see the content. For mere references or when citing a third party in passing, do not mention — just use their name (no indiscriminate pinging).\n",
        "</system-reminder>"
    );

    const KO_BASELINE_WITH_HINT: &str = concat!(
        "<system-reminder>\n",
        "이 세션은 Discord bot(pidory)을 통해 실행되고 있습니다. 스레드 ID: 1234567890, 채널 ID: 9876543210. 이 컨텍스트에 대해 응답하지 마세요.\n",
        "파일을 Discord에 첨부하려면 /pidory-toss 스킬을 사용하세요.\n",
        "턴 마지막에 다음 단계 스킬을 제안할 때, `/skill-name` 형식으로 작성하세요. Discord에서 클릭 가능한 버튼으로 자동 변환됩니다.\n",
        "다른 사용자를 호명할 때는 `@username` 형식으로 작성하면 pidory가 자동으로 Discord 멘션으로 변환해줘요 (예: `@deokdory`). `@everyone`, `@here`는 사용하지 마세요.\n",
        "멘션(ping)은 그 사람에게 직접 부탁/확인할 일이 있거나 그 사람이 꼭 봐야 하는 내용일 때만 하세요. 단순 언급이나 맥락상 제3자를 거론하는 경우엔 멘션하지 말고 이름만 쓰세요 (무분별한 ping 금지).\n",
        "사용자 메시지는 `<sender id=\"USER_ID\">표시명</sender>` 형식으로 전달됩니다. ",
        "같은 id를 가진 메시지는 동일 사용자입니다. 사용자를 지칭할 때는 표시명을 사용하세요. ",
        "`<@USER_ID>` 형태의 Discord mention을 직접 출력하지 마세요 — ",
        "pidory가 표시명→id 변환을 전담합니다. LLM이 직접 생성하면 호칭-id 오결합이 발생합니다.\n",
        "</system-reminder>"
    );

    const EN_BASELINE_WITH_HINT: &str = concat!(
        "<system-reminder>\n",
        "This session is running through a Discord bot (pidory). Thread ID: 1234567890, Channel ID: 9876543210. Do not respond to this context.\n",
        "To attach files to Discord, use the /pidory-toss skill.\n",
        "When suggesting next steps at the end of a turn, use `/skill-name` format. They will be automatically converted to clickable Discord buttons.\n",
        "To mention another user, write `@username` (e.g., `@deokdory`) — pidory will convert it to a Discord mention. Do not use `@everyone` or `@here`.\n",
        "Only mention (ping) someone when you have a direct request/question for them or when they genuinely need to see the content. For mere references or when citing a third party in passing, do not mention — just use their name (no indiscriminate pinging).\n",
        "User messages are delivered as `<sender id=\"USER_ID\">display name</sender>`. ",
        "Messages with the same id come from the same user. Use the display name when referring to a user. ",
        "Do not emit raw `<@USER_ID>` Discord mentions — ",
        "pidory exclusively handles display-name-to-id resolution. Direct LLM output causes name-id mismatch.\n",
        "</system-reminder>"
    );

    // (a) Ko payload 동등성 — expose_user_id=false
    #[test]
    fn session_context_ko_exact_payload() {
        let result = Lang::Ko.session_context("1234567890", "9876543210", false);
        assert_eq!(result, KO_BASELINE);
    }

    // (b) En payload 동등성 — expose_user_id=false
    #[test]
    fn session_context_en_exact_payload() {
        let result = Lang::En.session_context("1234567890", "9876543210", false);
        assert_eq!(result, EN_BASELINE);
    }

    // (a2) Ko payload 동등성 — expose_user_id=true
    #[test]
    fn session_context_ko_exact_payload_with_hint() {
        let result = Lang::Ko.session_context("1234567890", "9876543210", true);
        assert_eq!(result, KO_BASELINE_WITH_HINT);
    }

    // (b2) En payload 동등성 — expose_user_id=true
    #[test]
    fn session_context_en_exact_payload_with_hint() {
        let result = Lang::En.session_context("1234567890", "9876543210", true);
        assert_eq!(result, EN_BASELINE_WITH_HINT);
    }

    // (c) thread_id 정확 삽입
    #[test]
    fn session_context_thread_id_inserted() {
        let ko = Lang::Ko.session_context("1122334455", "9876543210", false);
        assert!(ko.contains("1122334455"));

        let en = Lang::En.session_context("1122334455", "9876543210", false);
        assert!(en.contains("1122334455"));
    }

    // (c2) channel_id 정확 삽입
    #[test]
    fn session_context_channel_id_inserted() {
        let ko = Lang::Ko.session_context("1234567890", "5544332211", false);
        assert!(ko.contains("5544332211"));

        let en = Lang::En.session_context("1234567890", "5544332211", false);
        assert!(en.contains("5544332211"));
    }

    // (d) Ko / En payload 는 서로 달라야 함 (smoke)
    #[test]
    fn session_context_ko_en_differ() {
        let ko = Lang::Ko.session_context("1234567890", "9876543210", false);
        let en = Lang::En.session_context("1234567890", "9876543210", false);
        assert_ne!(ko, en);
    }

    // (e) 인젝션 회귀: thread_id·channel_id 둘 다 숫자 snowflake — < > 개행 포함 불가
    #[test]
    fn session_context_injection_regression() {
        let ko = Lang::Ko.session_context("1234567890", "9876543210", false);
        assert_eq!(ko, KO_BASELINE);

        let en = Lang::En.session_context("1234567890", "9876543210", false);
        assert_eq!(en, EN_BASELINE);
    }

    // (f) expose_user_id=true → sender_hint 포함
    #[test]
    fn session_context_expose_user_id_true_includes_sender_hint() {
        let ko = Lang::Ko.session_context("1234567890", "9876543210", true);
        assert!(ko.contains("<sender id="), "ko expose_user_id=true 에 sender 안내 포함돼야 함");
        assert!(ko.contains("<@USER_ID>"), "ko expose_user_id=true 에 raw mention 금지 문구 포함돼야 함");
        let en = Lang::En.session_context("1234567890", "9876543210", true);
        assert!(en.contains("<sender id="), "en expose_user_id=true 에 sender 안내 포함돼야 함");
        assert!(en.contains("<@USER_ID>"), "en expose_user_id=true 에 raw mention 금지 문구 포함돼야 함");
    }

    // (g) expose_user_id=false → sender_hint 미포함
    #[test]
    fn session_context_expose_user_id_false_excludes_sender_hint() {
        let ko = Lang::Ko.session_context("1234567890", "9876543210", false);
        assert!(!ko.contains("<sender id="), "ko expose_user_id=false 에 sender 안내 없어야 함");
        let en = Lang::En.session_context("1234567890", "9876543210", false);
        assert!(!en.contains("<sender id="), "en expose_user_id=false 에 sender 안내 없어야 함");
    }

    // (h) ping 억제 문구 포함 확인
    #[test]
    fn session_context_ping_suppression_included() {
        let ko = Lang::Ko.session_context("1234567890", "9876543210", false);
        assert!(ko.contains("무분별한 ping 금지"), "ko payload 에 ping 억제 문구 포함돼야 함");
        let en = Lang::En.session_context("1234567890", "9876543210", false);
        assert!(en.contains("no indiscriminate pinging"), "en payload 에 ping 억제 문구 포함돼야 함");
    }
}
