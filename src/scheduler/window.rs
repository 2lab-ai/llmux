//! `QuotaWindow` — one rate-limit window (5h session or 7d weekly) with
//! wall-clock expiry: once `resets_at` passes, the window reads as empty.

use std::time::{Duration, SystemTime};

/// Where a window observation came from. Headers are authoritative during
/// traffic; the usage poller covers idle accounts. Freshest `fetched_at`
/// wins per window regardless of source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSource {
    /// Parsed from `anthropic-ratelimit-*` response headers.
    Headers,
    /// Fetched from `GET /api/oauth/usage`.
    UsagePoll,
}

/// A point-in-time observation of one quota window.
///
/// All time fields are `SystemTime` (wall clock), NOT `Instant`: reset
/// timestamps arrive as epoch seconds / RFC3339 from upstream and must
/// survive comparison against externally supplied "now" values in pure
/// scheduler code and in tests.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuotaWindow {
    /// Utilization 0.0..=1.0 as reported by upstream.
    pub utilization: f64,
    /// When this window resets (epoch-based wall clock).
    pub resets_at: SystemTime,
    /// When this observation was made; staleness is judged against this.
    pub fetched_at: SystemTime,
    pub source: WindowSource,
}

impl QuotaWindow {
    /// True once `resets_at` has passed — the window no longer constrains.
    pub fn is_expired(&self, now: SystemTime) -> bool {
        self.resets_at <= now
    }

    /// Utilization with wall-clock expiry applied: 0.0 if expired,
    /// `self.utilization` otherwise.
    pub fn effective_utilization(&self, now: SystemTime) -> f64 {
        if self.is_expired(now) {
            0.0
        } else {
            self.utilization
        }
    }

    /// True if this observation is older than `max_age`. Stale windows must
    /// not drive scheduling (don't schedule on fiction). An observation
    /// stamped in the future (clock skew) is treated as fresh.
    pub fn is_stale(&self, now: SystemTime, max_age: Duration) -> bool {
        now.duration_since(self.fetched_at)
            .is_ok_and(|age| age > max_age)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn window(utilization: f64, resets_at: u64, fetched_at: u64) -> QuotaWindow {
        QuotaWindow {
            utilization,
            resets_at: at(resets_at),
            fetched_at: at(fetched_at),
            source: WindowSource::Headers,
        }
    }

    #[test]
    fn not_expired_before_reset() {
        assert!(!window(0.5, 1000, 900).is_expired(at(999)));
    }

    #[test]
    fn expired_exactly_at_reset() {
        assert!(window(0.5, 1000, 900).is_expired(at(1000)));
    }

    #[test]
    fn expired_after_reset() {
        assert!(window(0.5, 1000, 900).is_expired(at(1001)));
    }

    #[test]
    fn effective_utilization_passes_through_when_live() {
        let w = window(0.73, 1000, 900);
        assert_eq!(w.effective_utilization(at(950)), 0.73);
    }

    #[test]
    fn effective_utilization_zero_when_expired() {
        let w = window(0.99, 1000, 900);
        assert_eq!(w.effective_utilization(at(1000)), 0.0);
    }

    #[test]
    fn stale_only_past_max_age() {
        let w = window(0.5, 10_000, 1000);
        let max_age = Duration::from_secs(600);
        assert!(!w.is_stale(at(1600), max_age), "age == max_age is fresh");
        assert!(w.is_stale(at(1601), max_age), "age > max_age is stale");
    }

    #[test]
    fn future_fetched_at_is_not_stale() {
        let w = window(0.5, 10_000, 5000);
        assert!(!w.is_stale(at(1000), Duration::from_secs(1)));
    }
}
