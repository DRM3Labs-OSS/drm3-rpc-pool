//! Per-endpoint health state and exponential backoff.
//!
//! Counts consecutive failures and gates retries on an exponentially-growing
//! cooldown. One success resets the counter.

use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;

/// Parameters for the backoff curve. Kept here so tests can construct a fast
/// variant without monkey-patching timing.
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    /// Failures at or below this count do NOT trigger demotion.
    pub demotion_threshold: u32,
    /// First retry gap once demoted.
    pub base_delay: Duration,
    /// Upper bound on the retry gap. Backoff doubles per failure but is capped
    /// here.
    pub max_delay: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            demotion_threshold: 2,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(300),
        }
    }
}

impl BackoffPolicy {
    /// Fast variant for tests. 1-failure demotion, 10ms base, 200ms cap.
    pub fn tight() -> Self {
        Self {
            demotion_threshold: 1,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(200),
        }
    }

    /// Current cooldown for a given failure count. Pure function of the policy
    /// plus the counter. `consecutive_failures = 0` returns `Duration::ZERO`.
    pub fn delay_for(&self, consecutive_failures: u32) -> Duration {
        if consecutive_failures == 0 {
            return Duration::ZERO;
        }
        // Over the demotion threshold we apply 2^(n - threshold) * base, up to max.
        let over = consecutive_failures.saturating_sub(self.demotion_threshold);
        // Saturating multiply avoids overflow at very high failure counts.
        let factor = 1u32.checked_shl(over).unwrap_or(u32::MAX);
        let scaled = self
            .base_delay
            .checked_mul(factor)
            .unwrap_or(self.max_delay);
        std::cmp::min(scaled, self.max_delay)
    }
}

/// Health state for a single endpoint.
///
/// All mutation goes through the `Mutex` so concurrent callers cannot race.
/// The critical sections are microseconds; contention is negligible relative
/// to the HTTP round-trip.
#[derive(Debug)]
pub struct EndpointHealth {
    inner: Mutex<Inner>,
    policy: BackoffPolicy,
}

#[derive(Debug, Clone)]
struct Inner {
    consecutive_failures: u32,
    last_success_at: Option<Instant>,
    last_failure_at: Option<Instant>,
    last_error: Option<String>,
    total_requests: u64,
    total_successes: u64,
    total_failures: u64,
    total_latency_ms: u128,
}

impl EndpointHealth {
    /// Fresh health with a given backoff policy.
    pub fn new(policy: BackoffPolicy) -> Self {
        Self {
            inner: Mutex::new(Inner {
                consecutive_failures: 0,
                last_success_at: None,
                last_failure_at: None,
                last_error: None,
                total_requests: 0,
                total_successes: 0,
                total_failures: 0,
                total_latency_ms: 0,
            }),
            policy,
        }
    }

    /// Whether the endpoint is currently eligible for dispatch.
    ///
    /// `now` is taken as an argument so time-dependent tests can control the
    /// clock without mocking the global one.
    pub fn is_available_at(&self, now: Instant) -> bool {
        let inner = self.inner.lock();
        if inner.consecutive_failures <= self.policy.demotion_threshold {
            return true;
        }
        let Some(last_failure) = inner.last_failure_at else {
            return true;
        };
        let cooldown = self.policy.delay_for(inner.consecutive_failures);
        now.saturating_duration_since(last_failure) >= cooldown
    }

    /// Convenience using `Instant::now()`.
    pub fn is_available(&self) -> bool {
        self.is_available_at(Instant::now())
    }

    /// Record a successful request. Resets consecutive_failures to 0.
    pub fn record_success(&self, latency: Duration) {
        let mut inner = self.inner.lock();
        inner.consecutive_failures = 0;
        inner.last_success_at = Some(Instant::now());
        inner.last_error = None;
        inner.total_requests += 1;
        inner.total_successes += 1;
        inner.total_latency_ms = inner.total_latency_ms.saturating_add(latency.as_millis());
    }

    /// Record a failed request. Increments consecutive_failures.
    pub fn record_failure(&self, error: impl Into<String>, latency: Duration) {
        let mut inner = self.inner.lock();
        inner.consecutive_failures = inner.consecutive_failures.saturating_add(1);
        inner.last_failure_at = Some(Instant::now());
        inner.last_error = Some(error.into());
        inner.total_requests += 1;
        inner.total_failures += 1;
        inner.total_latency_ms = inner.total_latency_ms.saturating_add(latency.as_millis());
    }

    /// Snapshot of the state suitable for JSON serialization (UI/metrics).
    pub fn snapshot(&self) -> EndpointStatus {
        let inner = self.inner.lock();
        EndpointStatus {
            consecutive_failures: inner.consecutive_failures,
            total_requests: inner.total_requests,
            total_successes: inner.total_successes,
            total_failures: inner.total_failures,
            average_latency_ms: if inner.total_requests == 0 {
                0.0
            } else {
                inner.total_latency_ms as f64 / inner.total_requests as f64
            },
            last_error: inner.last_error.clone(),
            last_success_age_ms: inner
                .last_success_at
                .map(|t| t.elapsed().as_millis() as u64),
            last_failure_age_ms: inner
                .last_failure_at
                .map(|t| t.elapsed().as_millis() as u64),
        }
    }

    /// Borrow the policy (for introspection and tests).
    pub fn policy(&self) -> &BackoffPolicy {
        &self.policy
    }
}

/// Serializable snapshot of endpoint health.
#[derive(Debug, Clone, Serialize)]
pub struct EndpointStatus {
    pub consecutive_failures: u32,
    pub total_requests: u64,
    pub total_successes: u64,
    pub total_failures: u64,
    pub average_latency_ms: f64,
    pub last_error: Option<String>,
    /// Milliseconds since the last successful request (None = never).
    pub last_success_age_ms: Option<u64>,
    /// Milliseconds since the last failed request (None = never).
    pub last_failure_age_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_for_zero_failures_is_zero() {
        let p = BackoffPolicy::default();
        assert_eq!(p.delay_for(0), Duration::ZERO);
    }

    #[test]
    fn delay_grows_exponentially_after_threshold() {
        let p = BackoffPolicy {
            demotion_threshold: 2,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(60),
        };
        // At the threshold (2), 2^0 * base.
        assert_eq!(p.delay_for(2), Duration::from_millis(100));
        assert_eq!(p.delay_for(3), Duration::from_millis(200));
        assert_eq!(p.delay_for(4), Duration::from_millis(400));
        assert_eq!(p.delay_for(5), Duration::from_millis(800));
    }

    #[test]
    fn delay_caps_at_max() {
        let p = BackoffPolicy {
            demotion_threshold: 0,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
        };
        // 2^20 * 100ms would be 104857600ms. Cap at 1s.
        assert_eq!(p.delay_for(20), Duration::from_secs(1));
    }

    #[test]
    fn fresh_endpoint_is_available() {
        let h = EndpointHealth::new(BackoffPolicy::default());
        assert!(h.is_available());
    }

    #[test]
    fn endpoint_below_threshold_stays_available() {
        let p = BackoffPolicy {
            demotion_threshold: 3,
            ..BackoffPolicy::default()
        };
        let h = EndpointHealth::new(p);
        h.record_failure("boom", Duration::from_millis(5));
        h.record_failure("boom", Duration::from_millis(5));
        assert!(h.is_available());
    }

    #[test]
    fn endpoint_past_threshold_is_unavailable_until_cooldown() {
        let p = BackoffPolicy {
            demotion_threshold: 1,
            base_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(60),
        };
        let h = EndpointHealth::new(p);
        h.record_failure("boom", Duration::from_millis(5));
        h.record_failure("boom", Duration::from_millis(5));
        let just_after = Instant::now();
        assert!(!h.is_available_at(just_after));
        // Beyond cooldown: available again.
        let later = Instant::now() + Duration::from_secs(120);
        assert!(h.is_available_at(later));
    }

    #[test]
    fn success_resets_consecutive_failures() {
        let p = BackoffPolicy::tight();
        let h = EndpointHealth::new(p);
        h.record_failure("boom", Duration::from_millis(5));
        h.record_failure("boom", Duration::from_millis(5));
        h.record_success(Duration::from_millis(5));
        let s = h.snapshot();
        assert_eq!(s.consecutive_failures, 0);
        assert_eq!(s.total_successes, 1);
        assert_eq!(s.total_failures, 2);
        assert!(s.last_error.is_none());
        assert!(h.is_available());
    }

    #[test]
    fn average_latency_is_tracked() {
        let h = EndpointHealth::new(BackoffPolicy::default());
        h.record_success(Duration::from_millis(100));
        h.record_success(Duration::from_millis(200));
        let s = h.snapshot();
        assert_eq!(s.total_requests, 2);
        assert!((s.average_latency_ms - 150.0).abs() < 0.1);
    }
}
