//! The RPC pool.
//!
//! Holds a priority-sorted list of endpoints and their health state. Dispatches
//! JSON-RPC calls by iterating candidates in priority order and returning on
//! the first success. Skips endpoints that do not declare the requested
//! capability and endpoints that are currently in cooldown.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::config::{RpcCapability, RpcEndpoint, RpcPoolConfig};
use crate::error::RpcError;
use crate::health::{BackoffPolicy, EndpointHealth, EndpointStatus};
use crate::metrics::Metrics;
use crate::transport::Transport;

#[cfg(any(feature = "reqwest-transport", test))]
use crate::metrics::NoopMetrics;
#[cfg(feature = "reqwest-transport")]
use crate::transport::ReqwestTransport;

mod rate_limit;
#[cfg(test)]
mod tests;
mod wire;

use rate_limit::RateLimiter;
use wire::{classify_response, parse_rpc_response, JsonRpcRequest, RawOutcome};

// ── Pool state ───────────────────────────────────────────────────────

struct PoolEntry {
    endpoint: RpcEndpoint,
    health: Arc<EndpointHealth>,
    /// Resolved auth headers, computed once at construction.
    auth_headers: Vec<(String, String)>,
    /// `None` when the endpoint has no `max_rps`.
    rate_limiter: Option<RateLimiter>,
}

/// The RPC pool. Clone is cheap — all state is behind `Arc`.
pub struct RpcPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    entries: Vec<PoolEntry>,
    transport: Arc<dyn Transport>,
    metrics: Arc<dyn Metrics>,
    request_id: AtomicU64,
    /// Max endpoints to attempt per call. `0` = try all candidates.
    max_retries: u32,
}

impl Clone for RpcPool {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl RpcPool {
    /// Construct a pool with an explicit transport, metrics, and backoff.
    pub fn new(
        config: RpcPoolConfig,
        transport: Arc<dyn Transport>,
        metrics: Arc<dyn Metrics>,
        backoff: BackoffPolicy,
    ) -> Result<Self, RpcError> {
        config.validate()?;
        let max_retries = config.max_retries;
        let mut endpoints = config.endpoints;
        // Sort by (priority asc, original index asc). Keep ties stable.
        endpoints.sort_by_key(|e| e.priority);
        let entries = endpoints
            .into_iter()
            .map(|endpoint| {
                let auth_headers = endpoint.auth.headers();
                let rate_limiter = endpoint.max_rps.map(RateLimiter::per_second);
                PoolEntry {
                    endpoint,
                    health: Arc::new(EndpointHealth::new(backoff.clone())),
                    auth_headers,
                    rate_limiter,
                }
            })
            .collect();

        Ok(Self {
            inner: Arc::new(PoolInner {
                entries,
                transport,
                metrics,
                request_id: AtomicU64::new(1),
                max_retries,
            }),
        })
    }

    /// Convenience — default metrics (no-op) and default backoff policy.
    #[cfg(feature = "reqwest-transport")]
    pub fn with_default_transport(config: RpcPoolConfig) -> Self {
        Self::new(
            config,
            Arc::new(ReqwestTransport::new()),
            Arc::new(NoopMetrics),
            BackoffPolicy::default(),
        )
        .expect("default pool construction should only fail on invalid config")
    }

    /// Build a pool straight from a parsed [`RpcPoolConfig`], honoring its
    /// `request_timeout_ms`. Uses the bundled reqwest transport and no-op
    /// metrics. This is what the proxy daemon uses.
    #[cfg(feature = "reqwest-transport")]
    pub fn from_config(config: RpcPoolConfig) -> Result<Self, RpcError> {
        let transport: Arc<dyn Transport> = match config.request_timeout_ms {
            Some(ms) => Arc::new(ReqwestTransport::with_timeout(Duration::from_millis(ms))),
            None => Arc::new(ReqwestTransport::new()),
        };
        Self::new(
            config,
            transport,
            Arc::new(NoopMetrics),
            BackoffPolicy::default(),
        )
    }

    /// Total endpoint count.
    pub fn len(&self) -> usize {
        self.inner.entries.len()
    }

    /// `true` if no endpoints are registered.
    pub fn is_empty(&self) -> bool {
        self.inner.entries.is_empty()
    }

    /// Endpoint definitions (in dispatch order).
    pub fn endpoints(&self) -> Vec<RpcEndpoint> {
        self.inner
            .entries
            .iter()
            .map(|e| e.endpoint.clone())
            .collect()
    }

    /// Full status snapshot (endpoint definition + live health).
    pub fn status(&self) -> Vec<PoolEntryStatus> {
        self.inner
            .entries
            .iter()
            .map(|e| PoolEntryStatus {
                endpoint: e.endpoint.clone(),
                health: e.health.snapshot(),
            })
            .collect()
    }

    /// Low-level access for tests: the health handle of the Nth entry.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn health_at(&self, index: usize) -> Option<Arc<EndpointHealth>> {
        self.inner.entries.get(index).map(|e| e.health.clone())
    }

    /// Dispatch a JSON-RPC call. Tries endpoints in priority order; first
    /// success wins.
    ///
    /// `params` must serialize to a JSON array per the JSON-RPC spec. Passing
    /// `json!(null)` or `json!({})` is allowed if the method expects it.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let capability = RpcCapability::for_method(method);
        let now = Instant::now();

        // Candidate selection. Walk in priority order (entries is already
        // sorted) and skip endpoints that are incapable or in cooldown.
        let mut attempts: Vec<(String, String)> = Vec::new();
        let mut considered = 0usize;
        let mut incapable = 0usize;
        let mut in_cooldown = 0usize;
        let mut rate_limited = 0usize;
        let mut tried = 0u32;

        for entry in &self.inner.entries {
            considered += 1;
            let tag = entry.endpoint.tag();
            if !entry.endpoint.supports(&capability) {
                self.inner.metrics.on_skipped_incapable(tag, method);
                incapable += 1;
                continue;
            }
            if !entry.health.is_available_at(now) {
                self.inner.metrics.on_skipped_cooldown(tag, method);
                in_cooldown += 1;
                continue;
            }
            // Per-endpoint client-side throttle. A locally-throttled endpoint is
            // skipped (not awaited) so the call fails over instead of stalling.
            if let Some(rl) = &entry.rate_limiter {
                if !rl.try_acquire() {
                    rate_limited += 1;
                    continue;
                }
            }
            // Retry budget: stop after max_retries dispatch attempts (0 = all).
            if self.inner.max_retries != 0 && tried >= self.inner.max_retries {
                break;
            }
            tried += 1;

            self.inner.metrics.on_request(tag, method);
            let id = self.inner.request_id.fetch_add(1, Ordering::Relaxed);
            let body = serde_json::to_vec(&JsonRpcRequest {
                jsonrpc: "2.0",
                id,
                method,
                params: params.clone(),
            })
            .map_err(|e| RpcError::Malformed {
                endpoint: entry.endpoint.url.clone(),
                message: format!("request serialize failed: {e}"),
            })?;

            let start = Instant::now();
            match self
                .inner
                .transport
                .post_json_with_headers(&entry.endpoint.url, &entry.auth_headers, body)
                .await
            {
                Ok(bytes) => {
                    let latency = start.elapsed();
                    match parse_rpc_response(&bytes) {
                        Ok(value) => {
                            entry.health.record_success(latency);
                            self.inner.metrics.on_success(tag, method, latency);
                            tracing::debug!(
                                endpoint = %tag,
                                method = %method,
                                latency_ms = %latency.as_millis(),
                                "rpc success"
                            );
                            return Ok(value);
                        }
                        Err(err) => {
                            let msg = err.to_string();
                            entry.health.record_failure(&msg, latency);
                            self.inner.metrics.on_failure(tag, method, latency, &msg);
                            tracing::warn!(
                                endpoint = %tag,
                                method = %method,
                                error = %msg,
                                "rpc parse/response error"
                            );
                            attempts.push((entry.endpoint.url.clone(), msg));
                            continue;
                        }
                    }
                }
                Err(transport_err) => {
                    let latency = start.elapsed();
                    entry.health.record_failure(&transport_err, latency);
                    self.inner
                        .metrics
                        .on_failure(tag, method, latency, &transport_err);
                    tracing::warn!(
                        endpoint = %tag,
                        method = %method,
                        error = %transport_err,
                        "rpc transport error"
                    );
                    attempts.push((entry.endpoint.url.clone(), transport_err));
                    continue;
                }
            }
        }

        if attempts.is_empty() {
            return Err(RpcError::NoCandidates {
                method: method.to_string(),
                reason: format!(
                    "considered={considered} incapable={incapable} in_cooldown={in_cooldown} rate_limited={rate_limited}"
                ),
            });
        }

        Err(RpcError::AllFailed {
            method: method.to_string(),
            count: attempts.len(),
            attempts,
        })
    }

    /// Forward a JSON-RPC call for the proxy daemon.
    ///
    /// Like [`call`](Self::call) it tries endpoints in priority order with
    /// auth, capability routing, rate limiting, health and backoff. The one
    /// difference: a well-formed JSON-RPC *error envelope* from an upstream
    /// (a revert, "method not found", bad params) is a valid answer and is
    /// returned via [`ForwardResult::Error`] rather than triggering failover.
    /// Only transport failures, non-2xx HTTP, and unparseable bodies fail over.
    pub async fn forward(&self, method: &str, params: Value) -> Result<ForwardResult, RpcError> {
        let capability = RpcCapability::for_method(method);
        let now = Instant::now();

        let mut attempts: Vec<(String, String)> = Vec::new();
        let mut considered = 0usize;
        let mut incapable = 0usize;
        let mut in_cooldown = 0usize;
        let mut rate_limited = 0usize;
        let mut tried = 0u32;

        for entry in &self.inner.entries {
            considered += 1;
            let tag = entry.endpoint.tag();
            if !entry.endpoint.supports(&capability) {
                self.inner.metrics.on_skipped_incapable(tag, method);
                incapable += 1;
                continue;
            }
            if !entry.health.is_available_at(now) {
                self.inner.metrics.on_skipped_cooldown(tag, method);
                in_cooldown += 1;
                continue;
            }
            if let Some(rl) = &entry.rate_limiter {
                if !rl.try_acquire() {
                    rate_limited += 1;
                    continue;
                }
            }
            if self.inner.max_retries != 0 && tried >= self.inner.max_retries {
                break;
            }
            tried += 1;

            self.inner.metrics.on_request(tag, method);
            let id = self.inner.request_id.fetch_add(1, Ordering::Relaxed);
            let body = serde_json::to_vec(&JsonRpcRequest {
                jsonrpc: "2.0",
                id,
                method,
                params: params.clone(),
            })
            .map_err(|e| RpcError::Malformed {
                endpoint: entry.endpoint.url.clone(),
                message: format!("request serialize failed: {e}"),
            })?;

            let start = Instant::now();
            match self
                .inner
                .transport
                .post_json_with_headers(&entry.endpoint.url, &entry.auth_headers, body)
                .await
            {
                Ok(bytes) => {
                    let latency = start.elapsed();
                    match classify_response(&bytes) {
                        Ok(RawOutcome::Result(value)) => {
                            entry.health.record_success(latency);
                            self.inner.metrics.on_success(tag, method, latency);
                            return Ok(ForwardResult::Result(value));
                        }
                        Ok(RawOutcome::RpcError(err)) => {
                            // A valid upstream answer. Count as success for
                            // health (the endpoint is up) and relay it.
                            entry.health.record_success(latency);
                            self.inner.metrics.on_success(tag, method, latency);
                            return Ok(ForwardResult::Error {
                                code: err.code,
                                message: err.message,
                                data: err.data,
                            });
                        }
                        Err(err) => {
                            let msg = err.to_string();
                            entry.health.record_failure(&msg, latency);
                            self.inner.metrics.on_failure(tag, method, latency, &msg);
                            attempts.push((entry.endpoint.url.clone(), msg));
                            continue;
                        }
                    }
                }
                Err(transport_err) => {
                    let latency = start.elapsed();
                    entry.health.record_failure(&transport_err, latency);
                    self.inner
                        .metrics
                        .on_failure(tag, method, latency, &transport_err);
                    attempts.push((entry.endpoint.url.clone(), transport_err));
                    continue;
                }
            }
        }

        if attempts.is_empty() {
            return Err(RpcError::NoCandidates {
                method: method.to_string(),
                reason: format!(
                    "considered={considered} incapable={incapable} in_cooldown={in_cooldown} rate_limited={rate_limited}"
                ),
            });
        }

        Err(RpcError::AllFailed {
            method: method.to_string(),
            count: attempts.len(),
            attempts,
        })
    }

    /// Dispatch with a hard deadline. Fails with a transport error if no
    /// endpoint responds in time. Provided for completeness; the inner
    /// transport is already expected to time out on its own.
    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Value, RpcError> {
        match tokio::time::timeout(deadline, self.call(method, params)).await {
            Ok(r) => r,
            Err(_) => Err(RpcError::Transport {
                endpoint: "<pool>".into(),
                message: format!("pool-wide timeout after {deadline:?}"),
            }),
        }
    }
}

/// JSON-friendly pool-entry status used by the RPC settings UI.
#[derive(Debug, Clone, Serialize)]
pub struct PoolEntryStatus {
    #[serde(flatten)]
    pub endpoint: RpcEndpoint,
    pub health: EndpointStatus,
}

/// Result of [`RpcPool::forward`]: either an upstream `result` or a relayed
/// JSON-RPC error envelope.
#[derive(Debug, Clone)]
pub enum ForwardResult {
    /// Upstream returned a `result`.
    Result(Value),
    /// Upstream returned a well-formed JSON-RPC error (relay verbatim).
    Error {
        code: i64,
        message: String,
        data: Option<Value>,
    },
}
