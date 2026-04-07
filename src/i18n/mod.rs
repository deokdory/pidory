use serde::Deserialize;

mod background;
mod commands;
mod formatting;
mod notification;
mod permission;
mod session;

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Ko,
    En,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_ko() {
        assert_eq!(Lang::default(), Lang::Ko);
    }

    #[test]
    fn deserialize_from_string() {
        let ko: Lang = serde_json::from_str(r#""ko""#).unwrap();
        assert_eq!(ko, Lang::Ko);
        let en: Lang = serde_json::from_str(r#""en""#).unwrap();
        assert_eq!(en, Lang::En);
    }

    #[test]
    fn session_evicted_both_langs() {
        assert!(Lang::Ko.session_evicted().contains("정리"));
        assert!(Lang::En.session_evicted().contains("evicted"));
    }

    #[test]
    fn format_relative_time_ko() {
        assert_eq!(Lang::Ko.format_relative_time(30), "30초 전");
        assert_eq!(Lang::Ko.format_relative_time(120), "2분 전");
        assert_eq!(Lang::Ko.format_relative_time(7200), "2시간 전");
        assert_eq!(Lang::Ko.format_relative_time(172800), "2일 전");
    }

    #[test]
    fn format_relative_time_en() {
        assert_eq!(Lang::En.format_relative_time(30), "30s ago");
        assert_eq!(Lang::En.format_relative_time(120), "2m ago");
        assert_eq!(Lang::En.format_relative_time(7200), "2h ago");
        assert_eq!(Lang::En.format_relative_time(172800), "2d ago");
    }

    #[test]
    fn format_idle_ko() {
        assert_eq!(Lang::Ko.format_idle(30), "유휴 30초");
        assert_eq!(Lang::Ko.format_idle(150), "유휴 2분");
        assert_eq!(Lang::Ko.format_idle(7200), "유휴 2시간0분");
        assert_eq!(Lang::Ko.format_idle(5430), "유휴 1시간30분");
    }

    #[test]
    fn format_idle_en() {
        assert_eq!(Lang::En.format_idle(30), "idle 30s");
        assert_eq!(Lang::En.format_idle(150), "idle 2m");
        assert_eq!(Lang::En.format_idle(7200), "idle 2h0m");
        assert_eq!(Lang::En.format_idle(5430), "idle 1h30m");
    }

    #[test]
    fn permission_labels_differ() {
        assert_ne!(Lang::Ko.btn_allow(), Lang::En.btn_allow());
        assert_ne!(Lang::Ko.btn_deny(), Lang::En.btn_deny());
    }

    #[test]
    fn format_with_args() {
        let err = "timeout";
        assert!(Lang::Ko.session_create_failed(&err).contains("timeout"));
        assert!(Lang::En.session_create_failed(&err).contains("timeout"));
    }

    #[test]
    fn timeout_messages_both_langs() {
        // soft_timeout_nudge returns non-empty strings
        assert!(!Lang::Ko.soft_timeout_nudge().is_empty());
        assert!(!Lang::En.soft_timeout_nudge().is_empty());

        // hard_timeout_kill returns non-empty strings
        assert!(!Lang::Ko.hard_timeout_kill().is_empty());
        assert!(!Lang::En.hard_timeout_kill().is_empty());

        // Ko and En variants are different from each other
        assert_ne!(Lang::Ko.soft_timeout_nudge(), Lang::En.soft_timeout_nudge());
        assert_ne!(Lang::Ko.hard_timeout_kill(), Lang::En.hard_timeout_kill());
    }

    #[test]
    fn session_context_ko() {
        let ctx = Lang::Ko.session_context("버그 수정");
        assert!(ctx.starts_with("<system-reminder>"));
        assert!(ctx.ends_with("</system-reminder>"));
        assert!(ctx.contains("pidory"));
        assert!(ctx.contains("버그 수정"));
        assert!(ctx.contains("pidory-toss"));
    }

    #[test]
    fn session_context_en() {
        let ctx = Lang::En.session_context("fix bug");
        assert!(ctx.starts_with("<system-reminder>"));
        assert!(ctx.ends_with("</system-reminder>"));
        assert!(ctx.contains("pidory"));
        assert!(ctx.contains("fix bug"));
        assert!(ctx.contains("pidory-toss"));
    }

    #[test]
    fn session_context_langs_differ() {
        let ko = Lang::Ko.session_context("test");
        let en = Lang::En.session_context("test");
        assert_ne!(ko, en);
    }
}
