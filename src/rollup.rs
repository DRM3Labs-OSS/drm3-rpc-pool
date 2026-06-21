//! Tumbling-window rollup metrics for the proxy.
//!
//! A [`Metrics`] implementation that accumulates per-endpoint outcomes, and a
//! [`RollupMetrics::flush`] that emits one `tracing` event per endpoint (count,
//! success rate, p50/p95 latency) for the window and then clears it. The proxy
//! flushes on a fixed interval, so `--log-format json` gets a periodic
//! statistical summary alongside the per-call events.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use crate::metrics::Metrics;

#[derive(Default)]
struct Window {
    ok: u64,
    err: u64,
    latencies_ms: Vec<u64>,
}

/// One endpoint's stats for a flushed window.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointRollup {
    pub endpoint: String,
    pub requests: u64,
    pub ok: u64,
    pub err: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
}

/// Accumulates per-endpoint outcomes for the current window. Cheap to clone the
/// `Arc`; all state is behind one mutex (critical sections are microseconds).
#[derive(Default)]
pub struct RollupMetrics {
    inner: Mutex<HashMap<String, Window>>,
}

impl RollupMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take and clear the current window, returning per-endpoint stats sorted by
    /// endpoint. Endpoints with no traffic are omitted.
    pub fn drain(&self) -> Vec<EndpointRollup> {
        let snapshot = {
            let mut map = self.inner.lock().expect("rollup mutex poisoned");
            std::mem::take(&mut *map)
        };
        let mut out: Vec<EndpointRollup> = snapshot
            .into_iter()
            .filter_map(|(endpoint, mut w)| {
                let requests = w.ok + w.err;
                if requests == 0 {
                    return None;
                }
                w.latencies_ms.sort_unstable();
                let pct = |q: f64| -> u64 {
                    if w.latencies_ms.is_empty() {
                        return 0;
                    }
                    let rank = (q * (w.latencies_ms.len() as f64 - 1.0)).round() as usize;
                    w.latencies_ms[rank.min(w.latencies_ms.len() - 1)]
                };
                Some(EndpointRollup {
                    endpoint,
                    requests,
                    ok: w.ok,
                    err: w.err,
                    p50_ms: pct(0.50),
                    p95_ms: pct(0.95),
                })
            })
            .collect();
        out.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
        out
    }

    /// Drain the window and emit one `rollup` tracing event per endpoint. Silent
    /// when no endpoint saw traffic in the window.
    pub fn flush(&self, window_secs: u64) {
        for r in self.drain() {
            let success_rate = if r.requests == 0 {
                0.0
            } else {
                r.ok as f64 / r.requests as f64
            };
            tracing::info!(
                endpoint = %r.endpoint,
                window_s = window_secs,
                requests = r.requests,
                ok = r.ok,
                err = r.err,
                success_rate = %format!("{success_rate:.3}"),
                p50_ms = r.p50_ms,
                p95_ms = r.p95_ms,
                "rollup"
            );
        }
    }
}

impl Metrics for RollupMetrics {
    fn on_request(&self, _endpoint_tag: &str, _method: &str) {}

    fn on_success(&self, endpoint_tag: &str, _method: &str, latency: Duration) {
        let mut map = self.inner.lock().expect("rollup mutex poisoned");
        let w = map.entry(endpoint_tag.to_string()).or_default();
        w.ok += 1;
        w.latencies_ms.push(latency.as_millis() as u64);
    }

    fn on_failure(&self, endpoint_tag: &str, _method: &str, latency: Duration, _error: &str) {
        let mut map = self.inner.lock().expect("rollup mutex poisoned");
        let w = map.entry(endpoint_tag.to_string()).or_default();
        w.err += 1;
        w.latencies_ms.push(latency.as_millis() as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_aggregates_then_clears() {
        let m = RollupMetrics::new();
        for ms in [10u64, 20, 30, 40] {
            m.on_success("a", "eth_blockNumber", Duration::from_millis(ms));
        }
        m.on_failure("a", "eth_blockNumber", Duration::from_millis(5), "boom");
        m.on_success("b", "eth_blockNumber", Duration::from_millis(100));

        let mut got = m.drain();
        got.sort_by(|x, y| x.endpoint.cmp(&y.endpoint));
        assert_eq!(got.len(), 2);

        let a = &got[0];
        assert_eq!(a.endpoint, "a");
        assert_eq!(a.requests, 5);
        assert_eq!(a.ok, 4);
        assert_eq!(a.err, 1);
        // 5 samples sorted [5,10,20,30,40]; p50 rank=2 -> 20, p95 rank=4 -> 40.
        assert_eq!(a.p50_ms, 20);
        assert_eq!(a.p95_ms, 40);

        // Window is cleared after a drain.
        assert!(m.drain().is_empty());
    }
}
