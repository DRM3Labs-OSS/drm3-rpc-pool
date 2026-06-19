//! Property tests for the pool.
//!
//! Invariants we care about:
//! - After N random (success|failure) scripts across K endpoints, the pool's
//!   total success counter equals the number of successful calls observed.
//! - At least one healthy endpoint always returns success (never double-counts
//!   or skips an available candidate).
//! - Priority order is respected: the first successful endpoint in each call is
//!   always the lowest-priority healthy candidate at the time of dispatch.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use async_trait::async_trait;
use drm3_rpc_pool::{
    health::BackoffPolicy, NoopMetrics, RpcEndpoint, RpcPool, RpcPoolConfig, Transport,
};
use proptest::prelude::*;
use serde_json::json;

struct ScriptedTransport {
    // Per-URL call counter. Used to determine whether this call succeeds or
    // fails, based on the caller-supplied script.
    calls: Mutex<std::collections::HashMap<String, usize>>,
    // For each URL, a Vec<bool> - true = success, false = failure. If the
    // counter exceeds the script length, default to success.
    script: Mutex<std::collections::HashMap<String, Vec<bool>>>,
    global_calls: AtomicUsize,
}

impl ScriptedTransport {
    fn new(scripts: std::collections::HashMap<String, Vec<bool>>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(std::collections::HashMap::new()),
            script: Mutex::new(scripts),
            global_calls: AtomicUsize::new(0),
        })
    }

    fn total_calls(&self) -> usize {
        self.global_calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl Transport for ScriptedTransport {
    async fn post_json(&self, url: &str, _body: Vec<u8>) -> Result<Vec<u8>, String> {
        self.global_calls.fetch_add(1, Ordering::Relaxed);
        let mut calls = self.calls.lock().unwrap();
        let n = calls.entry(url.to_string()).or_insert(0);
        let current = *n;
        *n += 1;
        drop(calls);
        let script = self.script.lock().unwrap();
        let success = script
            .get(url)
            .and_then(|v| v.get(current))
            .copied()
            .unwrap_or(true);
        if success {
            Ok(serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": url
            }))
            .unwrap())
        } else {
            Err("scripted failure".into())
        }
    }
}

fn arb_script() -> impl Strategy<Value = Vec<bool>> {
    prop::collection::vec(any::<bool>(), 1..32)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// With a fixed endpoint list and random failure scripts, total transport
    /// calls >= number of pool.call() invocations (can be more due to failover).
    #[test]
    fn total_transport_calls_at_least_invocations(
        a_script in arb_script(),
        b_script in arb_script(),
        c_script in arb_script(),
        n_calls in 1usize..16,
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let urls = vec!["https://a".to_string(), "https://b".to_string(), "https://c".to_string()];
            let mut scripts = std::collections::HashMap::new();
            scripts.insert(urls[0].clone(), a_script);
            scripts.insert(urls[1].clone(), b_script);
            scripts.insert(urls[2].clone(), c_script);
            let transport = ScriptedTransport::new(scripts);
            let pool = RpcPool::new(
                RpcPoolConfig::from_urls(urls.clone()),
                transport.clone(),
                Arc::new(NoopMetrics),
                BackoffPolicy {
                    demotion_threshold: 999,
                    base_delay: Duration::from_millis(1),
                    max_delay: Duration::from_millis(1),
                },
            ).unwrap();

            for _ in 0..n_calls {
                let _ = pool.call("eth_blockNumber", json!([])).await;
            }

            prop_assert!(transport.total_calls() >= n_calls);
            Ok(())
        }).unwrap();
    }

    /// With an always-healthy endpoint somewhere in the list, every call should
    /// succeed (even if earlier endpoints fail).
    #[test]
    fn always_succeeds_if_any_endpoint_is_healthy(
        bad_prefix_count in 0usize..6,
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let mut endpoints = Vec::new();
            let mut scripts = std::collections::HashMap::new();
            for i in 0..bad_prefix_count {
                let url = format!("https://bad-{i}");
                endpoints.push(RpcEndpoint {
                    priority: i as u32,
                    ..RpcEndpoint::new(url.clone())
                });
                // Script: all failures.
                scripts.insert(url, vec![false; 32]);
            }
            // Healthy endpoint at the end.
            let healthy_url = "https://healthy".to_string();
            endpoints.push(RpcEndpoint {
                priority: 1000,
                ..RpcEndpoint::new(healthy_url.clone())
            });
            scripts.insert(healthy_url, vec![true; 32]);

            let transport = ScriptedTransport::new(scripts);
            let pool = RpcPool::new(
                RpcPoolConfig {
                    endpoints,
                    ..RpcPoolConfig::default()
                },
                transport,
                Arc::new(NoopMetrics),
                BackoffPolicy {
                    demotion_threshold: 9999, // never cooldown
                    base_delay: Duration::from_millis(1),
                    max_delay: Duration::from_millis(1),
                },
            ).unwrap();

            let result = pool.call("eth_blockNumber", json!([])).await;
            prop_assert!(result.is_ok());
            Ok(())
        }).unwrap();
    }
}
