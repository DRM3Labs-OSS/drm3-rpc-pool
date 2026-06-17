//! A minimal per-endpoint token-bucket governor.
//!
//! Enforces `max_rps` without pulling an external crate. The bucket holds up to
//! `rps` tokens and refills continuously at `rps` tokens/second. `try_acquire`
//! is non-blocking: if no token is available the caller should skip the
//! endpoint and fail over, rather than stalling the whole request.

use std::time::Instant;

use parking_lot::Mutex;

#[derive(Debug)]
pub(super) struct RateLimiter {
    inner: Mutex<Inner>,
    capacity: f64,
    refill_per_sec: f64,
}

#[derive(Debug)]
struct Inner {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    /// Construct a limiter allowing `rps` requests per second (burst = `rps`).
    /// A zero `rps` is clamped to 1 to avoid a permanently-closed bucket.
    pub(super) fn per_second(rps: u32) -> Self {
        let rps = rps.max(1) as f64;
        Self {
            inner: Mutex::new(Inner {
                tokens: rps,
                last_refill: Instant::now(),
            }),
            capacity: rps,
            refill_per_sec: rps,
        }
    }

    /// Try to consume one token. Returns `true` if granted.
    pub(super) fn try_acquire(&self) -> bool {
        self.try_acquire_at(Instant::now())
    }

    /// Clock-injectable variant for deterministic tests.
    pub(super) fn try_acquire_at(&self, now: Instant) -> bool {
        let mut inner = self.inner.lock();
        let elapsed = now
            .saturating_duration_since(inner.last_refill)
            .as_secs_f64();
        if elapsed > 0.0 {
            inner.tokens = (inner.tokens + elapsed * self.refill_per_sec).min(self.capacity);
            inner.last_refill = now;
        }
        if inner.tokens >= 1.0 {
            inner.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn allows_up_to_capacity_then_blocks() {
        let rl = RateLimiter::per_second(3);
        let t0 = Instant::now();
        assert!(rl.try_acquire_at(t0));
        assert!(rl.try_acquire_at(t0));
        assert!(rl.try_acquire_at(t0));
        // Fourth in the same instant is denied.
        assert!(!rl.try_acquire_at(t0));
    }

    #[test]
    fn refills_over_time() {
        let rl = RateLimiter::per_second(2);
        let t0 = Instant::now();
        assert!(rl.try_acquire_at(t0));
        assert!(rl.try_acquire_at(t0));
        assert!(!rl.try_acquire_at(t0));
        // After one second, full bucket again.
        let t1 = t0 + Duration::from_secs(1);
        assert!(rl.try_acquire_at(t1));
        assert!(rl.try_acquire_at(t1));
    }

    #[test]
    fn zero_rps_clamped_to_one() {
        let rl = RateLimiter::per_second(0);
        let t0 = Instant::now();
        assert!(rl.try_acquire_at(t0));
        assert!(!rl.try_acquire_at(t0));
    }
}
