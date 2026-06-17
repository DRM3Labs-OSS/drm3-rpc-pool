//! Metrics callbacks.
//!
//! Operators plug in their own implementation (Prometheus, statsd, stdout,
//! whatever). A `NoopMetrics` is provided for pools that do not want metrics.

use std::time::Duration;

/// Hook fired by the pool at key lifecycle points.
///
/// `endpoint_tag` is either the endpoint label (if configured) or the URL —
/// same as `RpcEndpoint::tag()`.
pub trait Metrics: Send + Sync + 'static {
    /// Called just before a request is dispatched to a specific endpoint.
    fn on_request(&self, endpoint_tag: &str, method: &str);

    /// Called after a successful response.
    fn on_success(&self, endpoint_tag: &str, method: &str, latency: Duration);

    /// Called after a failed response (transport or JSON-RPC error).
    fn on_failure(&self, endpoint_tag: &str, method: &str, latency: Duration, error: &str);

    /// Called when an endpoint is skipped because it lacks the capability
    /// required by the method.
    fn on_skipped_incapable(&self, endpoint_tag: &str, method: &str) {
        let _ = (endpoint_tag, method);
    }

    /// Called when an endpoint is skipped because it is in cooldown.
    fn on_skipped_cooldown(&self, endpoint_tag: &str, method: &str) {
        let _ = (endpoint_tag, method);
    }
}

/// Default no-op implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;

impl Metrics for NoopMetrics {
    fn on_request(&self, _endpoint_tag: &str, _method: &str) {}
    fn on_success(&self, _endpoint_tag: &str, _method: &str, _latency: Duration) {}
    fn on_failure(&self, _endpoint_tag: &str, _method: &str, _latency: Duration, _error: &str) {}
}

#[cfg(test)]
pub(crate) mod testing {
    //! A recording metrics implementation for unit tests.

    use std::sync::Mutex;
    use std::time::Duration;

    use super::Metrics;

    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub enum Event {
        Request(String, String),
        Success(String, String, Duration),
        Failure(String, String, Duration, String),
        SkippedIncapable(String, String),
        SkippedCooldown(String, String),
    }

    #[derive(Debug, Default)]
    pub struct RecordingMetrics {
        pub events: Mutex<Vec<Event>>,
    }

    impl RecordingMetrics {
        pub fn events(&self) -> Vec<Event> {
            self.events.lock().unwrap().clone()
        }
    }

    impl Metrics for RecordingMetrics {
        fn on_request(&self, endpoint_tag: &str, method: &str) {
            self.events
                .lock()
                .unwrap()
                .push(Event::Request(endpoint_tag.into(), method.into()));
        }
        fn on_success(&self, endpoint_tag: &str, method: &str, latency: Duration) {
            self.events.lock().unwrap().push(Event::Success(
                endpoint_tag.into(),
                method.into(),
                latency,
            ));
        }
        fn on_failure(&self, endpoint_tag: &str, method: &str, latency: Duration, error: &str) {
            self.events.lock().unwrap().push(Event::Failure(
                endpoint_tag.into(),
                method.into(),
                latency,
                error.into(),
            ));
        }
        fn on_skipped_incapable(&self, endpoint_tag: &str, method: &str) {
            self.events
                .lock()
                .unwrap()
                .push(Event::SkippedIncapable(endpoint_tag.into(), method.into()));
        }
        fn on_skipped_cooldown(&self, endpoint_tag: &str, method: &str) {
            self.events
                .lock()
                .unwrap()
                .push(Event::SkippedCooldown(endpoint_tag.into(), method.into()));
        }
    }
}
