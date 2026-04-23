use crate::ratelimit::RateLimitInfo;

pub(super) fn handle_ratelimit_event(
    ratelimit_tx: &tokio::sync::watch::Sender<RateLimitInfo>,
    rate_limit_type: Option<&str>,
    utilization: Option<f64>,
    resets_at: Option<u64>,
    is_using_overage: Option<bool>,
) {
    if let Some(rlt) = rate_limit_type {
        let resets = resets_at.unwrap_or(0);
        let overage = is_using_overage.unwrap_or(false);
        if let Some(util) = utilization {
            ratelimit_tx.send_modify(|info| {
                info.update_from_event(rlt, util, resets, overage);
            });
        } else {
            ratelimit_tx.send_modify(|info| {
                info.update_resets_only(rlt, resets, overage);
            });
        }
    } else {
        tracing::debug!("rate_limit_event without rateLimitType: utilization={:?}", utilization);
    }
}
