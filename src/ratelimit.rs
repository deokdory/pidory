use poise::serenity_prelude::{ChannelId, Context};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitInfo {
    pub five_hour_pct: u8,
    pub seven_day_pct: u8,
    pub five_hour_reset: u64,
    pub seven_day_reset: u64,
    pub updated_at: u64,
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

        let five_h_remaining = if info.five_hour_reset > now {
            let diff = info.five_hour_reset - now;
            let h = diff / 3600;
            let m = (diff % 3600) / 60;
            format!("({}h{}m)", h, m)
        } else {
            String::new()
        };

        format!(
            "5h: {}%{} | 7d: {}%",
            info.five_hour_pct, five_h_remaining, info.seven_day_pct
        )
    }

    /// Returns the list of thresholds that should trigger alerts (not yet alerted in this reset
    /// cycle). Also updates internal state: clears alerted set on reset cycle change, records
    /// newly triggered thresholds.
    pub fn check_thresholds(&mut self, info: &RateLimitInfo) -> Vec<u8> {
        if info.five_hour_reset != self.last_five_hour_reset {
            self.alerted_thresholds.clear();
            self.last_five_hour_reset = info.five_hour_reset;
        }

        let mut triggered = Vec::new();
        for &threshold in &self.alert_thresholds {
            if info.five_hour_pct >= threshold && !self.alerted_thresholds.contains(&threshold) {
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
    ) {
        let triggered = self.check_thresholds(info);
        for threshold in triggered {
            let msg = format!(
                "⚠️ Rate limit alert: 5h usage at {}% (threshold: {}%)",
                info.five_hour_pct, threshold
            );
            if let Err(e) = channel_id.say(ctx, &msg).await {
                tracing::warn!("failed to send rate limit alert: {e}");
            }
        }
    }
}

pub fn read_ratelimit_file(path: &str) -> Option<RateLimitInfo> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("ratelimit file not found or unreadable: {path}: {e}");
            return None;
        }
    };

    match serde_json::from_str::<RateLimitInfo>(&content) {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::warn!("failed to parse ratelimit file {path}: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_info(five_hour_pct: u8, seven_day_pct: u8, five_hour_reset: u64) -> RateLimitInfo {
        RateLimitInfo {
            five_hour_pct,
            seven_day_pct,
            five_hour_reset,
            seven_day_reset: 456,
            updated_at: 789,
        }
    }

    #[test]
    fn test_read_valid_file() {
        let dir = std::env::temp_dir().join("pidory_test_ratelimit_valid");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("ratelimit.json");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            r#"{{"five_hour_pct":42,"seven_day_pct":38,"five_hour_reset":123,"seven_day_reset":456,"updated_at":789}}"#
        )
        .unwrap();

        let result = read_ratelimit_file(path.to_str().unwrap());
        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.five_hour_pct, 42);
        assert_eq!(info.seven_day_pct, 38);
        assert_eq!(info.five_hour_reset, 123);
        assert_eq!(info.seven_day_reset, 456);
        assert_eq!(info.updated_at, 789);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_read_missing_file() {
        let result = read_ratelimit_file("/nonexistent/path/ratelimit.json");
        assert!(result.is_none());
    }

    #[test]
    fn test_read_invalid_json() {
        let dir = std::env::temp_dir().join("pidory_test_ratelimit_invalid");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("bad.json");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, "not json").unwrap();

        let result = read_ratelimit_file(path.to_str().unwrap());
        assert!(result.is_none());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_format_presence_with_remaining() {
        // reset이 미래 시점이면 남은 시간 표시
        let future_reset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600 + 1800; // 1h30m from now
        let info = RateLimitInfo {
            five_hour_pct: 42,
            seven_day_pct: 38,
            five_hour_reset: future_reset,
            seven_day_reset: 456,
            updated_at: 789,
        };
        let s = RateLimitMonitor::format_presence(&info);
        assert!(s.starts_with("5h: 42%(1h"), "got: {s}");
        assert!(s.contains("| 7d: 38%"), "got: {s}");
    }

    #[test]
    fn test_format_presence_past_reset() {
        // reset이 과거면 남은 시간 없이 표시
        let info = make_info(42, 38, 123);
        let s = RateLimitMonitor::format_presence(&info);
        assert_eq!(s, "5h: 42% | 7d: 38%");
    }

    #[test]
    fn test_alert_threshold_triggered() {
        let mut monitor = RateLimitMonitor::new(vec![50]);
        let info = make_info(55, 0, 100);
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
        let info = make_info(55, 0, 100);

        let first = monitor.check_thresholds(&info);
        assert_eq!(first, vec![50]);

        let second = monitor.check_thresholds(&info);
        assert!(second.is_empty(), "same threshold must not trigger twice in same reset cycle");
    }

    #[test]
    fn test_alert_reset_cycle() {
        let mut monitor = RateLimitMonitor::new(vec![50]);
        let info1 = make_info(55, 0, 100);
        let first = monitor.check_thresholds(&info1);
        assert_eq!(first, vec![50]);

        // same reset — deduped
        let dedup = monitor.check_thresholds(&info1);
        assert!(dedup.is_empty());

        // new reset cycle (five_hour_reset changed) — alerted_thresholds should clear
        let info2 = make_info(55, 0, 200);
        let after_reset = monitor.check_thresholds(&info2);
        assert_eq!(after_reset, vec![50], "new reset cycle must re-trigger threshold");
    }
}
