use poise::serenity_prelude::{ChannelId, Context};

use crate::i18n::Lang;

#[derive(Debug, Clone, Default)]
pub struct RateLimitInfo {
    pub five_hour_pct: Option<u8>,
    pub seven_day_pct: Option<u8>,
    pub five_hour_reset: Option<u64>,
    pub seven_day_reset: Option<u64>,
    pub is_using_overage: bool,
    pub updated_at: u64,
}

impl RateLimitInfo {
    pub fn update_from_event(
        &mut self,
        rate_limit_type: &str,
        utilization: f64,
        resets_at: u64,
        is_overage: bool,
    ) {
        let pct = (utilization * 100.0).round() as u8;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        match rate_limit_type {
            "five_hour" => {
                self.five_hour_pct = Some(pct);
                self.five_hour_reset = Some(resets_at);
            }
            "seven_day" => {
                self.seven_day_pct = Some(pct);
                self.seven_day_reset = Some(resets_at);
            }
            _ => {
                tracing::warn!("unknown rate_limit_type: {}", rate_limit_type);
                return;
            }
        }
        self.is_using_overage = is_overage;
        self.updated_at = now;
    }

    /// utilization이 없는 이벤트용: reset/overage/updated_at만 갱신, pct 유지
    pub fn update_resets_only(
        &mut self,
        rate_limit_type: &str,
        resets_at: u64,
        is_overage: bool,
    ) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        match rate_limit_type {
            "five_hour" => {
                self.five_hour_reset = Some(resets_at);
            }
            "seven_day" => {
                self.seven_day_reset = Some(resets_at);
            }
            _ => {
                tracing::warn!("unknown rate_limit_type: {}", rate_limit_type);
                return;
            }
        }
        self.is_using_overage = is_overage;
        self.updated_at = now;
    }
}

pub struct RateLimitMonitor {
    last_five_hour_reset: u64,
    last_seven_day_reset: u64,
    last_notified_five_hour_pct: Option<u8>,
    last_notified_seven_day_pct: Option<u8>,
}

impl RateLimitMonitor {
    pub fn new() -> Self {
        Self {
            last_five_hour_reset: 0,
            last_seven_day_reset: 0,
            last_notified_five_hour_pct: None,
            last_notified_seven_day_pct: None,
        }
    }

    /// Format a notification message for a rate limit window.
    ///
    /// Example: `"⚠️ Rate Limit: 5h 91% (resets in 4h 59m)"`
    pub fn format_notification(rate_limit_type: &str, pct: u8, resets_at: Option<u64>) -> String {
        let label = match rate_limit_type {
            "five_hour" => "5h",
            "seven_day" => "7d",
            other => other,
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let remaining = resets_at
            .filter(|&r| r > now)
            .map(|r| {
                let diff = r - now;
                let h = diff / 3600;
                let m = (diff % 3600) / 60;
                format!(" (resets in {}h{}m)", h, m)
            })
            .unwrap_or_default();

        format!("⚠️ Rate Limit: {} {}%{}", label, pct, remaining)
    }

    /// Send a Discord notification if the rate limit percentage changed.
    ///
    /// Detects reset-cycle changes and clears the "last notified" state so the
    /// new cycle re-fires even if the percentage happens to be the same value.
    pub async fn notify_if_changed(
        &mut self,
        info: &RateLimitInfo,
        ctx: &Context,
        channel_id: ChannelId,
        _lang: Lang,
    ) {
        // ── 5h window ──────────────────────────────────────────────────────────
        let five_hour_reset = info.five_hour_reset.unwrap_or(0);
        if five_hour_reset != self.last_five_hour_reset {
            self.last_five_hour_reset = five_hour_reset;
            self.last_notified_five_hour_pct = None;
        }
        if let Some(pct) = info.five_hour_pct
            && self.last_notified_five_hour_pct != Some(pct)
        {
            let msg = Self::format_notification("five_hour", pct, info.five_hour_reset);
            if let Err(e) = channel_id.say(&ctx.http, &msg).await {
                tracing::warn!("failed to send rate limit notification (5h): {e}");
            } else {
                self.last_notified_five_hour_pct = Some(pct);
            }
        }

        // ── 7d window ──────────────────────────────────────────────────────────
        let seven_day_reset = info.seven_day_reset.unwrap_or(0);
        if seven_day_reset != self.last_seven_day_reset {
            self.last_seven_day_reset = seven_day_reset;
            self.last_notified_seven_day_pct = None;
        }
        if let Some(pct) = info.seven_day_pct
            && self.last_notified_seven_day_pct != Some(pct)
        {
            let msg = Self::format_notification("seven_day", pct, info.seven_day_reset);
            if let Err(e) = channel_id.say(&ctx.http, &msg).await {
                tracing::warn!("failed to send rate limit notification (7d): {e}");
            } else {
                self.last_notified_seven_day_pct = Some(pct);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_from_event_five_hour() {
        let mut info = RateLimitInfo::default();
        info.update_from_event("seven_day", 0.57, 999999, false);
        info.update_from_event("five_hour", 0.24, 888888, false);
        assert_eq!(info.five_hour_pct, Some(24));
        assert_eq!(info.five_hour_reset, Some(888888));
        assert_eq!(info.seven_day_pct, Some(57));
        assert_eq!(info.seven_day_reset, Some(999999));
    }

    #[test]
    fn test_update_from_event_seven_day() {
        let mut info = RateLimitInfo::default();
        info.update_from_event("five_hour", 0.24, 888888, false);
        info.update_from_event("seven_day", 0.57, 999999, false);
        assert_eq!(info.seven_day_pct, Some(57));
        assert_eq!(info.seven_day_reset, Some(999999));
        assert_eq!(info.five_hour_pct, Some(24));
        assert_eq!(info.five_hour_reset, Some(888888));
    }

    #[test]
    fn test_update_from_event_unknown_type() {
        let mut info = RateLimitInfo::default();
        info.update_from_event("unknown", 0.5, 12345, false);
        assert_eq!(info.five_hour_pct, None);
        assert_eq!(info.seven_day_pct, None);
        assert_eq!(info.updated_at, 0);
    }

    #[test]
    fn test_format_notification_five_hour_with_remaining() {
        let future_reset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600 + 1800; // 1h30m from now
        let s = RateLimitMonitor::format_notification("five_hour", 91, Some(future_reset));
        assert!(s.starts_with("⚠️ Rate Limit: 5h 91%"), "got: {s}");
        assert!(s.contains("resets in 1h"), "got: {s}");
    }

    #[test]
    fn test_format_notification_seven_day_no_remaining() {
        let s = RateLimitMonitor::format_notification("seven_day", 75, None);
        assert_eq!(s, "⚠️ Rate Limit: 7d 75%");
    }

    #[test]
    fn test_format_notification_past_reset() {
        let past = 100u64; // clearly in the past
        let s = RateLimitMonitor::format_notification("five_hour", 60, Some(past));
        assert_eq!(s, "⚠️ Rate Limit: 5h 60%");
    }

    #[test]
    fn test_monitor_new_has_no_notified_state() {
        let monitor = RateLimitMonitor::new();
        assert_eq!(monitor.last_notified_five_hour_pct, None);
        assert_eq!(monitor.last_notified_seven_day_pct, None);
        assert_eq!(monitor.last_five_hour_reset, 0);
        assert_eq!(monitor.last_seven_day_reset, 0);
    }
}
