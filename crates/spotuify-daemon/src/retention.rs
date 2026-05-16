use spotuify_spotify::config::AnalyticsConfig;

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetentionCutoffs {
    pub(crate) progress_ms: i64,
    pub(crate) events_ms: i64,
    pub(crate) operations_ms: i64,
}

pub(crate) fn retention_cutoffs(
    now_ms: i64,
    analytics: Option<&AnalyticsConfig>,
) -> RetentionCutoffs {
    let defaults = AnalyticsConfig::default();
    let analytics = analytics.unwrap_or(&defaults);
    RetentionCutoffs {
        progress_ms: cutoff(now_ms, analytics.retention_progress_days),
        events_ms: cutoff(now_ms, analytics.retention_events_days),
        operations_ms: cutoff(now_ms, analytics.retention_operations_days),
    }
}

fn cutoff(now_ms: i64, days: u32) -> i64 {
    now_ms.saturating_sub(i64::from(days).saturating_mul(DAY_MS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_cutoffs_honor_configured_windows() {
        let config = AnalyticsConfig {
            retention_progress_days: 10,
            retention_events_days: 20,
            retention_operations_days: 30,
            ..AnalyticsConfig::default()
        };
        let now = 1_000 * 86_400_000;

        let cutoffs = retention_cutoffs(now, Some(&config));

        assert_eq!(cutoffs.progress_ms, now - 10 * 86_400_000);
        assert_eq!(cutoffs.events_ms, now - 20 * 86_400_000);
        assert_eq!(cutoffs.operations_ms, now - 30 * 86_400_000);
    }
}
