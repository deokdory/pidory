use crate::i18n::Lang;

/// CLI 커맨드 문자열을 `<command-name>/cmd</command-name>` 형태로 포맷한다.
/// command에서 선행 `/`를 제거 후 `/cmd` 형태로 재조립.
/// args가 Some("") 이면 None과 동일하게 처리 — `<command-message>` 태그 생략.
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
    } else {
        content.to_string()
    }
}
