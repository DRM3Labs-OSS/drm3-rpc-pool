//! Pool dispatch / failover / capability-routing tests.

use super::*;
use crate::config::{RpcCapability, RpcEndpoint, RpcPoolConfig};
use crate::metrics::testing::{Event, RecordingMetrics};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering as AtomicOrdering},
    Mutex,
};

// ── Fake transport ────────────────────────────────────────────

/// Records every call and returns a scripted response per URL.
type HandlerFn = Box<dyn Fn(&str, &[u8]) -> Result<Vec<u8>, String> + Send + Sync>;

struct FakeTransport {
    calls: Mutex<Vec<(String, Vec<u8>)>>,
    handler: HandlerFn,
}

impl FakeTransport {
    fn new<F>(handler: F) -> Arc<Self>
    where
        F: Fn(&str, &[u8]) -> Result<Vec<u8>, String> + Send + Sync + 'static,
    {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            handler: Box::new(handler),
        })
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn urls_called(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.0.clone())
            .collect()
    }
}

#[async_trait]
impl Transport for FakeTransport {
    async fn post_json(&self, url: &str, body: Vec<u8>) -> Result<Vec<u8>, String> {
        self.calls.lock().unwrap().push((url.into(), body.clone()));
        (self.handler)(url, &body)
    }
}

fn ok_body(result: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":result})).unwrap()
}

fn rpc_error_body(code: i64, message: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "jsonrpc":"2.0",
        "id":1,
        "error": {"code": code, "message": message}
    }))
    .unwrap()
}

fn cfg(urls: &[&str]) -> RpcPoolConfig {
    RpcPoolConfig::from_urls(urls.iter().map(|s| s.to_string()))
}

// ── Basic dispatch ───────────────────────────────────────────

#[tokio::test]
async fn first_endpoint_success_returns_immediately() {
    let transport = FakeTransport::new(|_url, _body| Ok(ok_body(json!("0x1"))));
    let pool = RpcPool::new(
        cfg(&["https://a", "https://b"]),
        transport.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let result = pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(result, json!("0x1"));
    assert_eq!(transport.call_count(), 1);
    assert_eq!(transport.urls_called(), vec!["https://a"]);
}

#[tokio::test]
async fn rotates_past_failing_endpoint() {
    let transport = FakeTransport::new(|url, _body| {
        if url == "https://a" {
            Err("connection refused".into())
        } else {
            Ok(ok_body(json!("0x2")))
        }
    });
    let pool = RpcPool::new(
        cfg(&["https://a", "https://b"]),
        transport.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let result = pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(result, json!("0x2"));
    assert_eq!(transport.urls_called(), vec!["https://a", "https://b"]);
}

#[tokio::test]
async fn all_endpoints_fail_returns_all_failed_error() {
    let transport = FakeTransport::new(|_url, _body| Err("boom".into()));
    let pool = RpcPool::new(
        cfg(&["https://a", "https://b", "https://c"]),
        transport,
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let err = pool.call("eth_blockNumber", json!([])).await.unwrap_err();
    match err {
        RpcError::AllFailed {
            count, attempts, ..
        } => {
            assert_eq!(count, 3);
            assert_eq!(attempts.len(), 3);
        }
        other => panic!("expected AllFailed, got {other:?}"),
    }
}

// ── Priority ordering ────────────────────────────────────────

#[tokio::test]
async fn priority_overrides_insertion_order() {
    let transport = FakeTransport::new(|_url, _body| Ok(ok_body(json!("ok"))));
    let config = RpcPoolConfig {
        endpoints: vec![
            RpcEndpoint {
                priority: 10,
                ..RpcEndpoint::new("https://slow")
            },
            RpcEndpoint {
                priority: 1,
                ..RpcEndpoint::new("https://fast")
            },
        ],
        ..RpcPoolConfig::default()
    };
    let pool = RpcPool::new(
        config,
        transport.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(transport.urls_called(), vec!["https://fast"]);
}

// ── Capability routing ───────────────────────────────────────

#[tokio::test]
async fn capability_mismatch_skips_endpoint() {
    let transport = FakeTransport::new(|url, _body| {
        if url == "https://logs-capable" {
            Ok(ok_body(json!([])))
        } else {
            panic!("eth_getLogs should not have reached {url}");
        }
    });
    let config = RpcPoolConfig {
        endpoints: vec![
            RpcEndpoint {
                priority: 0,
                capabilities: vec![RpcCapability::EthCall],
                ..RpcEndpoint::new("https://call-only")
            },
            RpcEndpoint {
                priority: 1,
                capabilities: vec![RpcCapability::EthCall, RpcCapability::EthGetLogs],
                ..RpcEndpoint::new("https://logs-capable")
            },
        ],
        ..RpcPoolConfig::default()
    };
    let metrics = Arc::new(RecordingMetrics::default());
    let pool = RpcPool::new(
        config,
        transport.clone(),
        metrics.clone(),
        BackoffPolicy::tight(),
    )
    .unwrap();
    pool.call("eth_getLogs", json!([])).await.unwrap();
    assert_eq!(transport.urls_called(), vec!["https://logs-capable"]);
    let events = metrics.events();
    assert!(events.iter().any(|e| matches!(e,
            Event::SkippedIncapable(tag, method) if tag == "https://call-only" && method == "eth_getLogs"
        )));
}

#[tokio::test]
async fn no_candidates_error_when_no_endpoint_supports_method() {
    let transport = FakeTransport::new(|_url, _body| Ok(ok_body(json!("unreachable"))));
    let config = RpcPoolConfig {
        endpoints: vec![RpcEndpoint {
            priority: 0,
            capabilities: vec![RpcCapability::EthCall],
            ..RpcEndpoint::new("https://call-only")
        }],
        ..RpcPoolConfig::default()
    };
    let pool = RpcPool::new(
        config,
        transport,
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let err = pool.call("eth_getLogs", json!([])).await.unwrap_err();
    assert!(matches!(err, RpcError::NoCandidates { .. }));
}

// ── Health + backoff integration ─────────────────────────────

#[tokio::test]
async fn repeated_failures_trigger_cooldown() {
    let fail_count = Arc::new(AtomicUsize::new(0));
    let fail_count_clone = fail_count.clone();
    let transport = FakeTransport::new(move |url, _body| {
        if url == "https://a" {
            fail_count_clone.fetch_add(1, AtomicOrdering::Relaxed);
            Err("busted".into())
        } else {
            Ok(ok_body(json!("ok")))
        }
    });
    let pool = RpcPool::new(
        cfg(&["https://a", "https://b"]),
        transport,
        Arc::new(NoopMetrics),
        BackoffPolicy {
            // Threshold = 0 so a single failure immediately demotes the
            // endpoint for the cooldown window. Cooldown of 1h prevents
            // recovery within the test.
            demotion_threshold: 0,
            base_delay: Duration::from_secs(3600),
            max_delay: Duration::from_secs(3600),
        },
    )
    .unwrap();
    // First call: a fails once, b succeeds.
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    // Second call: a is in cooldown, should skip straight to b without
    // touching a again.
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(fail_count.load(AtomicOrdering::Relaxed), 1);
}

#[tokio::test]
async fn success_resets_consecutive_failures() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = calls.clone();
    let transport = FakeTransport::new(move |_url, _body| {
        let n = calls_clone.fetch_add(1, AtomicOrdering::Relaxed);
        if n == 0 {
            Err("first fails".into())
        } else {
            Ok(ok_body(json!("ok")))
        }
    });
    let pool = RpcPool::new(
        cfg(&["https://a"]),
        transport,
        Arc::new(NoopMetrics),
        BackoffPolicy {
            demotion_threshold: 5,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        },
    )
    .unwrap();
    // First call fails, but threshold = 5 so endpoint stays available.
    let _ = pool.call("eth_blockNumber", json!([])).await;
    let _ = pool.call("eth_blockNumber", json!([])).await;
    let status = pool.status();
    assert_eq!(status[0].health.total_successes, 1);
    assert_eq!(status[0].health.consecutive_failures, 0);
}

#[tokio::test]
async fn jsonrpc_error_body_counts_as_failure() {
    let transport = FakeTransport::new(|_url, _body| Ok(rpc_error_body(-32000, "rate limited")));
    let pool = RpcPool::new(
        cfg(&["https://a", "https://b"]),
        transport.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let err = pool.call("eth_blockNumber", json!([])).await.unwrap_err();
    assert!(matches!(err, RpcError::AllFailed { .. }));
    assert_eq!(transport.call_count(), 2);
}

// ── Metrics ──────────────────────────────────────────────────

#[tokio::test]
async fn metrics_hooks_fire_on_success_and_failure() {
    let metrics = Arc::new(RecordingMetrics::default());
    let transport = FakeTransport::new(|url, _body| {
        if url == "https://a" {
            Err("nope".into())
        } else {
            Ok(ok_body(json!("0x1")))
        }
    });
    let pool = RpcPool::new(
        cfg(&["https://a", "https://b"]),
        transport,
        metrics.clone(),
        BackoffPolicy::tight(),
    )
    .unwrap();
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    let events = metrics.events();
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::Request(tag, _) if tag == "https://a")));
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::Failure(tag, _, _, _) if tag == "https://a")));
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::Success(tag, _, _) if tag == "https://b")));
}

// ── Concurrency ──────────────────────────────────────────────

#[tokio::test]
async fn concurrent_calls_are_safe() {
    let transport = FakeTransport::new(|_url, _body| Ok(ok_body(json!("0x1"))));
    let pool = RpcPool::new(
        cfg(&["https://a", "https://b", "https://c"]),
        transport.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let mut joins = Vec::new();
    for _ in 0..100 {
        let p = pool.clone();
        joins.push(tokio::spawn(async move {
            p.call("eth_blockNumber", json!([])).await.unwrap();
        }));
    }
    for j in joins {
        j.await.unwrap();
    }
    let status = pool.status();
    let total_success: u64 = status.iter().map(|s| s.health.total_successes).sum();
    assert_eq!(total_success, 100);
}

// ── Validation / construction ────────────────────────────────

#[tokio::test]
async fn empty_config_is_rejected() {
    let transport = FakeTransport::new(|_url, _body| Ok(ok_body(json!("x"))));
    let err = RpcPool::new(
        RpcPoolConfig::default(),
        transport,
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    );
    assert!(matches!(err, Err(RpcError::Config(_))));
}

#[tokio::test]
async fn call_with_timeout_returns_pool_timeout_error() {
    // Transport that never completes.
    struct HangTransport;
    #[async_trait]
    impl Transport for HangTransport {
        async fn post_json(&self, _url: &str, _body: Vec<u8>) -> Result<Vec<u8>, String> {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }
    let pool = RpcPool::new(
        cfg(&["https://a"]),
        Arc::new(HangTransport),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let err = pool
        .call_with_timeout("eth_blockNumber", json!([]), Duration::from_millis(50))
        .await
        .unwrap_err();
    match err {
        RpcError::Transport { endpoint, .. } => assert_eq!(endpoint, "<pool>"),
        other => panic!("expected transport timeout, got {other:?}"),
    }
}

// ── Auth header dispatch ─────────────────────────────────────

/// Records the headers each call was made with.
struct HeaderRecordingTransport {
    headers: Mutex<Vec<Vec<(String, String)>>>,
}

impl HeaderRecordingTransport {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            headers: Mutex::new(Vec::new()),
        })
    }
    fn last_headers(&self) -> Vec<(String, String)> {
        self.headers
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl Transport for HeaderRecordingTransport {
    async fn post_json(&self, _url: &str, _body: Vec<u8>) -> Result<Vec<u8>, String> {
        self.headers.lock().unwrap().push(Vec::new());
        Ok(ok_body(json!("0x1")))
    }
    async fn post_json_with_headers(
        &self,
        _url: &str,
        headers: &[(String, String)],
        _body: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        self.headers.lock().unwrap().push(headers.to_vec());
        Ok(ok_body(json!("0x1")))
    }
}

fn endpoint_with_auth(url: &str, auth: crate::config::Auth) -> RpcPoolConfig {
    RpcPoolConfig {
        endpoints: vec![RpcEndpoint::new(url).with_auth(auth)],
        ..RpcPoolConfig::default()
    }
}

#[tokio::test]
async fn auth_none_sends_no_headers() {
    let t = HeaderRecordingTransport::new();
    let pool = RpcPool::new(
        endpoint_with_auth("https://a", crate::config::Auth::None),
        t.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert!(t.last_headers().is_empty());
}

#[tokio::test]
async fn auth_url_key_sends_no_headers() {
    let t = HeaderRecordingTransport::new();
    let pool = RpcPool::new(
        endpoint_with_auth("https://a/v2/key", crate::config::Auth::UrlKey),
        t.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert!(t.last_headers().is_empty());
}

#[tokio::test]
async fn auth_custom_header_is_sent() {
    let t = HeaderRecordingTransport::new();
    let pool = RpcPool::new(
        endpoint_with_auth(
            "https://a",
            crate::config::Auth::Header {
                name: "X-API-Key".into(),
                value: "secret".into(),
            },
        ),
        t.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(
        t.last_headers(),
        vec![("X-API-Key".into(), "secret".into())]
    );
}

#[tokio::test]
async fn auth_bearer_is_sent() {
    let t = HeaderRecordingTransport::new();
    let pool = RpcPool::new(
        endpoint_with_auth(
            "https://a",
            crate::config::Auth::Bearer {
                token: "tok123".into(),
            },
        ),
        t.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(
        t.last_headers(),
        vec![("Authorization".into(), "Bearer tok123".into())]
    );
}

// ── max_rps governor ─────────────────────────────────────────

#[tokio::test]
async fn max_rps_throttles_and_fails_over() {
    // First endpoint capped at 1 rps; second is unlimited. Burst of calls
    // should drain the first's single token, then fail over to the second.
    let transport = FakeTransport::new(|url, _body| {
        if url == "https://capped" {
            Ok(ok_body(json!("from-capped")))
        } else {
            Ok(ok_body(json!("from-spare")))
        }
    });
    let cfg = RpcPoolConfig {
        endpoints: vec![
            RpcEndpoint {
                max_rps: Some(1),
                priority: 0,
                ..RpcEndpoint::new("https://capped")
            },
            RpcEndpoint {
                priority: 1,
                ..RpcEndpoint::new("https://spare")
            },
        ],
        ..RpcPoolConfig::default()
    };
    let pool = RpcPool::new(
        cfg,
        transport.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();

    // First call uses the capped endpoint's only token.
    let r1 = pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(r1, json!("from-capped"));
    // Immediately again: capped is out of tokens, fails over to spare.
    let r2 = pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(r2, json!("from-spare"));
}

#[tokio::test]
async fn max_rps_only_one_endpoint_yields_no_candidates_when_drained() {
    let transport = FakeTransport::new(|_url, _body| Ok(ok_body(json!("ok"))));
    let cfg = RpcPoolConfig {
        endpoints: vec![RpcEndpoint {
            max_rps: Some(1),
            ..RpcEndpoint::new("https://solo")
        }],
        ..RpcPoolConfig::default()
    };
    let pool = RpcPool::new(
        cfg,
        transport,
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    pool.call("eth_blockNumber", json!([])).await.unwrap();
    let err = pool.call("eth_blockNumber", json!([])).await.unwrap_err();
    match err {
        RpcError::NoCandidates { reason, .. } => assert!(reason.contains("rate_limited=1")),
        other => panic!("expected NoCandidates, got {other:?}"),
    }
}

// ── max_retries budget ───────────────────────────────────────

#[tokio::test]
async fn max_retries_caps_attempts() {
    // Three failing endpoints, but max_retries = 1 means only one is tried.
    let transport = FakeTransport::new(|_url, _body| Err("boom".into()));
    let cfg = RpcPoolConfig {
        max_retries: 1,
        endpoints: vec![
            RpcEndpoint {
                priority: 0,
                ..RpcEndpoint::new("https://a")
            },
            RpcEndpoint {
                priority: 1,
                ..RpcEndpoint::new("https://b")
            },
            RpcEndpoint {
                priority: 2,
                ..RpcEndpoint::new("https://c")
            },
        ],
        ..RpcPoolConfig::default()
    };
    let pool = RpcPool::new(
        cfg,
        transport.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let err = pool.call("eth_blockNumber", json!([])).await.unwrap_err();
    match err {
        RpcError::AllFailed { count, .. } => assert_eq!(count, 1),
        other => panic!("expected AllFailed, got {other:?}"),
    }
    assert_eq!(transport.call_count(), 1);
}

// ── forward() relay semantics ────────────────────────────────

#[tokio::test]
async fn forward_relays_rpc_error_without_failover() {
    // First endpoint returns a JSON-RPC error envelope (a revert). forward()
    // must relay it and NOT fail over to the second.
    let transport = FakeTransport::new(|url, _body| {
        if url == "https://a" {
            Ok(rpc_error_body(3, "execution reverted"))
        } else {
            Ok(ok_body(json!("should-not-reach")))
        }
    });
    let cfg = RpcPoolConfig {
        endpoints: vec![
            RpcEndpoint {
                priority: 0,
                ..RpcEndpoint::new("https://a")
            },
            RpcEndpoint {
                priority: 1,
                ..RpcEndpoint::new("https://b")
            },
        ],
        ..RpcPoolConfig::default()
    };
    let pool = RpcPool::new(
        cfg,
        transport.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let r = pool.forward("eth_call", json!([])).await.unwrap();
    match r {
        ForwardResult::Error { code, message, .. } => {
            assert_eq!(code, 3);
            assert_eq!(message, "execution reverted");
        }
        other => panic!("expected relayed Error, got {other:?}"),
    }
    assert_eq!(transport.call_count(), 1);
}

#[tokio::test]
async fn forward_fails_over_on_transport_error() {
    let transport = FakeTransport::new(|url, _body| {
        if url == "https://a" {
            Err("connection refused".into())
        } else {
            Ok(ok_body(json!("0xrecovered")))
        }
    });
    let cfg = RpcPoolConfig {
        endpoints: vec![
            RpcEndpoint {
                priority: 0,
                ..RpcEndpoint::new("https://a")
            },
            RpcEndpoint {
                priority: 1,
                ..RpcEndpoint::new("https://b")
            },
        ],
        ..RpcPoolConfig::default()
    };
    let pool = RpcPool::new(
        cfg,
        transport.clone(),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let r = pool.forward("eth_blockNumber", json!([])).await.unwrap();
    match r {
        ForwardResult::Result(v) => assert_eq!(v, json!("0xrecovered")),
        other => panic!("expected Result, got {other:?}"),
    }
    assert_eq!(transport.call_count(), 2);
}
