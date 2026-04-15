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

pub enum CtxDisplayMode {
    Pending,
    Accurate(u8),
    Approximate(u8),
}

impl CtxDisplayMode {
    pub fn from_tokens(input_tokens: u64, context_window: u64, is_accurate: bool) -> Self {
        if context_window == 0 {
            if is_accurate {
                return Self::Accurate(0);
            } else {
                return Self::Approximate(0);
            }
        }
        let pct = ((input_tokens as f64 / context_window as f64 * 100.0).min(100.0)) as u8;
        if is_accurate { Self::Accurate(pct) } else { Self::Approximate(pct) }
    }
}

pub(crate) fn format_ctx_suffix(mode: CtxDisplayMode) -> String {
    match mode {
        CtxDisplayMode::Pending => " · ctx:-%".to_string(),
        CtxDisplayMode::Accurate(pct) => {
            if pct == 0 {
                return String::new();
            }
            format!(" · ctx:{pct}%")
        }
        CtxDisplayMode::Approximate(pct) => {
            if pct == 0 {
                return String::new();
            }
            format!(" · ctx:~{pct}%")
        }
    }
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
