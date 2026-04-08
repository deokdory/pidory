use crate::i18n::Lang;

pub(super) fn format_ctx_suffix(input_tokens: u64, context_window: u64) -> String {
    if context_window == 0 {
        return String::new();
    }
    let pct = (input_tokens as f64 / context_window as f64 * 100.0).min(100.0) as u8;
    format!(" ctx:{}%", pct)
}

/// `/new` 또는 `/clear` — 대화 컨텍스트를 리셋하는 명령인지 판정
pub(super) fn is_context_reset_command(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.eq_ignore_ascii_case("/new") || trimmed.eq_ignore_ascii_case("/clear")
}

/// 순수 함수: context inject 판정 및 content 생성
pub(super) fn build_context_content(
    content: &str,
    is_new_session: bool,
    had_needs_context: bool,
    thread_name: &str,
    lang: Lang,
) -> String {
    let is_new_command = is_context_reset_command(content);
    if !is_new_command && (is_new_session || had_needs_context) {
        let context = lang.session_context(thread_name);
        format!("{}\n\n{}", context, content)
    } else if is_new_command {
        "/compact".to_string()
    } else {
        content.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn test_new_command_becomes_compact() {
        let result = build_context_content("/new", false, false, "thread", Lang::Ko);
        assert_eq!(result, "/compact");
    }

    #[test]
    fn test_clear_command_becomes_compact() {
        let result = build_context_content("/clear", false, false, "thread", Lang::Ko);
        assert_eq!(result, "/compact");
    }

    #[test]
    fn test_mixed_case_new_becomes_compact() {
        let result = build_context_content("/New", false, false, "thread", Lang::Ko);
        assert_eq!(result, "/compact");
    }

    #[test]
    fn test_mixed_case_clear_becomes_compact() {
        let result = build_context_content("/CLEAR", false, false, "thread", Lang::Ko);
        assert_eq!(result, "/compact");
    }

    #[test]
    fn test_new_command_with_new_session_becomes_compact() {
        // context inject 안 됨 — /compact 반환
        let result = build_context_content("/new", true, false, "thread", Lang::Ko);
        assert_eq!(result, "/compact");
    }

    #[test]
    fn test_regular_message_passthrough() {
        let result = build_context_content("hello", false, false, "thread", Lang::Ko);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_regular_message_with_new_session_injects_context() {
        let result = build_context_content("hello", true, false, "my-thread", Lang::Ko);
        assert!(result.contains("hello"));
        assert!(result.contains("my-thread"));
    }

    #[test]
    fn test_regular_message_with_needs_context_injects_context() {
        let result = build_context_content("hello", false, true, "my-thread", Lang::Ko);
        assert!(result.contains("hello"));
        assert!(result.contains("my-thread"));
    }
}
