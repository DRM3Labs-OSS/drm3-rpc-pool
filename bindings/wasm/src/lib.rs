//! WASM binding for `drm3-rpc-pool`.
//!
//! Exposes a JS/TS-facing [`RpcPool`] class that runs the pool's failover,
//! per-endpoint health, backoff, capability routing, and rate limiting entirely
//! in wasm, while the network itself is done by the platform `fetch` (browser,
//! Web Worker, or Node 18+).
//!
//! ```js
//! import init, { RpcPool } from "@drm3labs-oss/rpc-pool";
//! await init();
//! const pool = new RpcPool({
//!   endpoints: [
//!     { url: "https://eth.llamarpc.com", priority: 0 },
//!     { url: "https://rpc.ankr.com/eth", priority: 1 },
//!   ],
//! });
//! const block = await pool.call("eth_blockNumber", []);
//! ```

mod fetch_transport;

use std::sync::Arc;

use drm3_rpc_pool::{BackoffPolicy, NoopMetrics, RpcPool as CoreRpcPool, RpcPoolConfig, Transport};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::prelude::*;

use fetch_transport::FetchTransport;

/// Mirror of [`RpcPoolConfig`] for JS-side input. We re-shape it here rather
/// than deserializing `RpcPoolConfig` directly so the daemon-only `listen`
/// field is optional and irrelevant browser-side, and so unknown JS fields are
/// rejected loudly.
#[derive(Deserialize)]
struct JsConfig {
    #[serde(default)]
    request_timeout_ms: Option<u64>,
    #[serde(default)]
    max_retries: Option<u32>,
    endpoints: Vec<Value>,
}

/// Install a panic hook so Rust panics surface as readable console errors.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// A resilient JSON-RPC failover pool, driven by the platform `fetch`.
#[wasm_bindgen]
pub struct RpcPool {
    inner: CoreRpcPool,
}

#[wasm_bindgen]
impl RpcPool {
    /// Construct a pool from a config object:
    ///
    /// ```ts
    /// new RpcPool({
    ///   request_timeout_ms?: number,   // currently advisory; see notes
    ///   max_retries?: number,          // 0 = try every healthy candidate
    ///   endpoints: Array<{
    ///     url: string,
    ///     label?: string,
    ///     priority?: number,           // lower tried first
    ///     capabilities?: string[],     // e.g. ["eth_call"]; empty = all
    ///     max_rps?: number,
    ///     auth?:
    ///       | { type: "none" }
    ///       | { type: "url_key" }
    ///       | { type: "header", name: string, value: string }
    ///       | { type: "bearer", token: string },
    ///   }>,
    /// })
    /// ```
    ///
    /// Throws if the config is malformed or the endpoint list is empty/has
    /// duplicate URLs.
    #[wasm_bindgen(constructor)]
    pub fn new(config: JsValue) -> Result<RpcPool, JsError> {
        let parsed: JsConfig = serde_wasm_bindgen::from_value(config)
            .map_err(|e| JsError::new(&format!("invalid config: {e}")))?;

        // Re-encode endpoints through serde_json so we reuse the core's
        // RpcEndpoint deserialization (capabilities, auth tagging, defaults).
        let endpoints = parsed
            .endpoints
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| JsError::new(&format!("invalid endpoint: {e}")))?;

        let cfg = RpcPoolConfig {
            request_timeout_ms: parsed.request_timeout_ms,
            max_retries: parsed.max_retries.unwrap_or(0),
            endpoints,
            ..RpcPoolConfig::default()
        };

        let transport: Arc<dyn Transport> = Arc::new(FetchTransport::new());
        let inner = CoreRpcPool::new(
            cfg,
            transport,
            Arc::new(NoopMetrics),
            BackoffPolicy::default(),
        )
        .map_err(|e| JsError::new(&e.to_string()))?;

        Ok(RpcPool { inner })
    }

    /// Dispatch a JSON-RPC call. Resolves to the JSON-RPC `result`, failing
    /// over across endpoints on transport errors / 429 / non-2xx. Rejects with
    /// an `Error` if every candidate fails.
    ///
    /// `params` must be a JSON-serializable value (array per the spec, but any
    /// JSON value is accepted).
    #[wasm_bindgen]
    pub async fn call(&self, method: String, params: JsValue) -> Result<JsValue, JsError> {
        let params: Value = if params.is_undefined() || params.is_null() {
            Value::Array(vec![])
        } else {
            serde_wasm_bindgen::from_value(params)
                .map_err(|e| JsError::new(&format!("invalid params: {e}")))?
        };

        let result = self
            .inner
            .call(&method, params)
            .await
            .map_err(|e| JsError::new(&e.to_string()))?;

        // json_compatible(), NOT the default to_value: the default serializes
        // JSON objects as JS `Map`s, so consumers doing `log.topics` or
        // `receipt.status` get `undefined`. json_compatible emits plain objects,
        // which is what every JSON-RPC caller expects.
        result
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|e| JsError::new(&format!("result serialize failed: {e}")))
    }

    /// Number of configured endpoints.
    #[wasm_bindgen(getter)]
    pub fn length(&self) -> usize {
        self.inner.len()
    }

    /// Live per-endpoint status snapshot (definition + health) as a JS value.
    #[wasm_bindgen]
    pub fn status(&self) -> Result<JsValue, JsError> {
        let snapshot = self.inner.status();
        snapshot
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .map_err(|e| JsError::new(&format!("status serialize failed: {e}")))
    }
}

/// The bundled free-peer list for a chain, as an array of `{ url, priority, ... }`
/// endpoint objects ready to drop straight into `new RpcPool({ endpoints })`.
///
/// Chains: `"base"`, `"ethereum"` (aka `"eth"`/`"mainnet"`), `"arbitrum"`,
/// `"optimism"`, `"polygon"`, `"bnb"`. Unknown chain -> empty array.
///
/// ```js
/// import { RpcPool, peersFor } from "@drm3labs-oss/rpc-pool";
/// const pool = new RpcPool({ endpoints: peersFor("base") });
/// ```
#[wasm_bindgen(js_name = peersFor)]
pub fn peers_for(chain: &str) -> Result<JsValue, JsError> {
    let endpoints = drm3_rpc_pool::presets::endpoints_for(chain).unwrap_or_default();
    endpoints
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|e| JsError::new(&format!("peersFor serialize failed: {e}")))
}
