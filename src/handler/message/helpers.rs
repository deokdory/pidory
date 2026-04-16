use crate::i18n::Lang;

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub(crate) fn format_cli_command(command: &str, args: Option<&str>) -> String {
    let cmd = escape_xml(command.trim_start_matches('/'));
    let base = format!("<command-name>/{cmd}</command-name>");
    match args {
        Some(a) if !a.is_empty() => {
            let escaped = escape_xml(a);
            format!("{base}<command-message>{escaped}</command-message>")
        }
        _ => base,
    }
}

pub(crate) fn shorten_model_name(model: &str) -> String {
    let base = model.split('@').next().unwrap_or(model);
    match base {
        s if s.starts_with("claude-opus") => "opus".into(),
        s if s.starts_with("claude-sonnet") => "sonnet".into(),
        s if s.starts_with("claude-haiku") => "haiku".into(),
        other => other.to_string(),
    }
}

pub(crate) fn format_ctx_suffix(input_tokens: u64, context_window: u64) -> String {
    if context_window == 0 {
        return String::new();
    }
    let pct = (input_tokens as f64 / context_window as f64 * 100.0).min(100.0) as u8;
    format!(" · ctx:{}%", pct)
}

/// `/compact [instructions]` 명령 파싱
///
/// - `/compact`가 아니면 `None`
/// - `/compact`(인자 없음)이면 `Some(None)`
/// - `/compact focus on auth`이면 `Some(Some("focus on auth"))`
pub(super) fn parse_compact_command(content: &str) -> Option<Option<&str>> {
    let trimmed = content.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower == "/compact" {
        return Some(None);
    }
    // `/compact` 뒤에 공백/탭이 있어야 인자로 인식
    if lower.starts_with("/compact") {
        let after = &trimmed["/compact".len()..];
        if after.starts_with(|c: char| c == ' ' || c == '\t') {
            let args = after.trim();
            if args.is_empty() {
                return Some(None);
            }
            return Some(Some(args));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_compact_basic() {
        assert_eq!(parse_compact_command("/compact"), Some(None));
    }

    #[test]
    fn parse_compact_case_insensitive() {
        assert_eq!(parse_compact_command("/Compact"), Some(None));
        assert_eq!(parse_compact_command("/COMPACT"), Some(None));
    }

    #[test]
    fn parse_compact_with_whitespace() {
        assert_eq!(parse_compact_command("  /compact  "), Some(None));
    }

    #[test]
    fn parse_compact_with_args() {
        assert_eq!(
            parse_compact_command("/compact Focus on auth"),
            Some(Some("Focus on auth"))
        );
    }

    #[test]
    fn parse_compact_with_tab_args() {
        assert_eq!(
            parse_compact_command("/compact\tkeep context"),
            Some(Some("keep context"))
        );
    }

    #[test]
    fn parse_compact_similar_not_compact() {
        assert_eq!(parse_compact_command("/compaction"), None);
    }

    #[test]
    fn parse_compact_empty() {
        assert_eq!(parse_compact_command(""), None);
    }

    #[test]
    fn parse_compact_regular_message() {
        assert_eq!(parse_compact_command("hello"), None);
    }
}

/// 순수 함수: context inject 판정 및 content 생성
pub(super) fn build_context_content(
    content: &str,
    is_new_session: bool,
    had_needs_context: bool,
    thread_name: &str,
    lang: Lang,
) -> String {
    if is_new_session || had_needs_context {
        let context = lang.session_context(thread_name);
        format!("{}\n\n{}", context, content)
    } else {
        content.to_string()
    }
}
