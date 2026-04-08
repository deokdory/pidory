use poise::serenity_prelude::{ChannelId, Context};
use std::collections::HashSet;

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
            _ => return,
        }
        self.is_using_overage = is_overage;
        self.updated_at = now;
    }
}

pub struct RateLimitMonitor {
    alert_thresholds: Vec<u8>,
    alerted_thresholds: HashSet<u8>,
    last_five_hour_reset: u64,
}

impl RateLimitMonitor {
    pub fn new(alert_thresholds: Vec<u8>) -> Self {
        Self {
            alert_thresholds,
            alerted_thresholds: HashSet::new(),
            last_five_hour_reset: 0,
        }
    }

    pub fn format_presence(info: &RateLimitInfo) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut parts = Vec::new();

        if let Some(pct) = info.five_hour_pct {
            let effective_pct = if info.five_hour_reset.map_or(true, |r| r <= now) {
                0
            } else {
                pct
            };
            let remaining = if let Some(reset) = info.five_hour_reset {
                if reset > now {
                    let diff = reset - now;
                    let h = diff / 3600;
                    let m = (diff % 3600) / 60;
                    format!("({}h{}m)", h, m)
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            if info.is_using_overage && effective_pct >= 100 {
                parts.push(format!("5h: {}%+{}", effective_pct, remaining));
            } else {
                parts.push(format!("5h: {}%{}", effective_pct, remaining));
            }
        }

        if let Some(pct) = info.seven_day_pct {
            let effective_pct = if info.seven_day_reset.map_or(true, |r| r <= now) {
                0
            } else {
                pct
            };
            if info.is_using_overage && effective_pct >= 100 {
                parts.push(format!("7d: {}%+", effective_pct));
            } else {
                parts.push(format!("7d: {}%", effective_pct));
            }
        }

        parts.join(" | ")
    }

    /// Returns the list of thresholds that should trigger alerts (not yet alerted in this reset
    /// cycle). Also updates internal state: clears alerted set on reset cycle change, records
    /// newly triggered thresholds.
    pub fn check_thresholds(&mut self, info: &RateLimitInfo) -> Vec<u8> {
        let five_hour_reset = info.five_hour_reset.unwrap_or(0);
        if five_hour_reset != self.last_five_hour_reset {
            self.alerted_thresholds.clear();
            self.last_five_hour_reset = five_hour_reset;
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // reset이 과거면 0%로 간주 (stale reset에 대한 false alert 방지)
        let pct = if info.five_hour_reset.map_or(true, |r| r <= now) {
            0
        } else {
            info.five_hour_pct.unwrap_or(0)
        };

        let mut triggered = Vec::new();
        for &threshold in &self.alert_thresholds {
            if pct >= threshold && !self.alerted_thresholds.contains(&threshold) {
                self.alerted_thresholds.insert(threshold);
                triggered.push(threshold);
            }
        }
        triggered
    }

    pub async fn check_and_alert(
        &mut self,
        info: &RateLimitInfo,
        ctx: &Context,
        channel_id: ChannelId,
        lang: Lang,
    ) {
        let triggered = self.check_thresholds(info);
        for threshold in triggered {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let remaining = if let Some(reset) = info.five_hour_reset {
                if reset > now {
                    let diff = reset - now;
                    let h = diff / 3600;
                    let m = (diff % 3600) / 60;
                    lang.resets_in(h, m)
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            let msg = lang.rate_limit_alert(info.five_hour_pct.unwrap_or(0), threshold, &remaining);
            if let Err(e) = channel_id.say(ctx, &msg).await {
                tracing::warn!("failed to send rate limit alert: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_info(five_hour_pct: u8, seven_day_pct: u8, five_hour_reset: u64) -> RateLimitInfo {
        RateLimitInfo {
            five_hour_pct: Some(five_hour_pct),
            seven_day_pct: Some(seven_day_pct),
            five_hour_reset: Some(five_hour_reset),
            seven_day_reset: Some(456),
            is_using_overage: false,
            updated_at: 789,
        }
    }

    #[test]
    fn test_format_presence_with_remaining() {
        let future_reset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600 + 1800;
        let info = RateLimitInfo {
            five_hour_pct: Some(42),
            seven_day_pct: Some(38),
            five_hour_reset: Some(future_reset),
            seven_day_reset: Some(future_reset + 86400),
            is_using_overage: false,
            updated_at: 789,
        };
        let s = RateLimitMonitor::format_presence(&info);
        assert!(s.starts_with("5h: 42%(1h"), "got: {s}");
        assert!(s.contains("| 7d: 38%"), "got: {s}");
    }

    #[test]
    fn test_format_presence_past_reset() {
        let info = make_info(42, 38, 123);
        let s = RateLimitMonitor::format_presence(&info);
        assert_eq!(s, "5h: 0% | 7d: 0%");
    }

    #[test]
    fn test_format_presence_default() {
        let info = RateLimitInfo::default();
        let s = RateLimitMonitor::format_presence(&info);
        assert_eq!(s, "");
    }

    #[test]
    fn test_format_presence_partial() {
        let future_reset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let info = RateLimitInfo {
            five_hour_pct: Some(42),
            seven_day_pct: None,
            five_hour_reset: Some(future_reset),
            seven_day_reset: None,
            is_using_overage: false,
            updated_at: 789,
        };
        let s = RateLimitMonitor::format_presence(&info);
        assert!(s.starts_with("5h: 42%"), "got: {s}");
        assert!(!s.contains("7d"), "got: {s}");
    }

    #[test]
    fn test_format_presence_stale_reset() {
        let info = RateLimitInfo {
            five_hour_pct: Some(50),
            seven_day_pct: Some(30),
            five_hour_reset: Some(100),
            seven_day_reset: Some(200),
            is_using_overage: false,
            updated_at: 789,
        };
        let s = RateLimitMonitor::format_presence(&info);
        assert_eq!(s, "5h: 0% | 7d: 0%");
    }

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
    fn test_alert_threshold_triggered() {
        let mut monitor = RateLimitMonitor::new(vec![50]);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut info = make_info(55, 0, 100);
        info.five_hour_reset = Some(now + 3600); // future reset
        let triggered = monitor.check_thresholds(&info);
        assert_eq!(triggered, vec![50]);
    }

    #[test]
    fn test_alert_threshold_not_triggered_below() {
        let mut monitor = RateLimitMonitor::new(vec![50]);
        let info = make_info(49, 0, 100);
        let triggered = monitor.check_thresholds(&info);
        assert!(triggered.is_empty());
    }

    #[test]
    fn test_alert_threshold_dedup() {
        let mut monitor = RateLimitMonitor::new(vec![50]);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut info = make_info(55, 0, 100);
        info.five_hour_reset = Some(now + 3600); // future reset

        let first = monitor.check_thresholds(&info);
        assert_eq!(first, vec![50]);

        let second = monitor.check_thresholds(&info);
        assert!(second.is_empty(), "same threshold must not trigger twice in same reset cycle");
    }

    #[test]
    fn test_alert_reset_cycle() {
        let mut monitor = RateLimitMonitor::new(vec![50]);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut info1 = make_info(55, 0, 100);
        info1.five_hour_reset = Some(now + 3600);
        let first = monitor.check_thresholds(&info1);
        assert_eq!(first, vec![50]);

        let dedup = monitor.check_thresholds(&info1);
        assert!(dedup.is_empty());

        let mut info2 = make_info(55, 0, 200);
        info2.five_hour_reset = Some(now + 7200); // different reset time
        let after_reset = monitor.check_thresholds(&info2);
        assert_eq!(after_reset, vec![50], "new reset cycle must re-trigger threshold");
    }
}
