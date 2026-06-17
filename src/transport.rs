//! Pluggable HTTP transport for the RPC pool.
//!
//! The pool does not care how bytes move — it only needs something that, given
//! a JSON-RPC request as bytes, produces a response body as bytes or an error.
//! The built-in `ReqwestTransport` is feature-gated so environments that bring
//! their own HTTP stack can disable it.

use async_trait::async_trait;

pub use crate::send_sync::MaybeSendSync;

/// Minimal HTTP transport contract.
///
/// Implementations must:
/// - Send `body` as the POST body with `Content-Type: application/json`.
/// - Return the raw response bytes on 2xx.
/// - Return `Err(..)` on any transport failure or non-2xx status.
///
/// On native targets the trait (and the futures it returns) are `Send + Sync`
/// so the pool can be shared across threads/tasks. On `wasm32` the browser
/// `fetch` future is not `Send` (it holds `JsValue`s), so the bounds are
/// relaxed and `async_trait` is asked to emit `?Send` futures. Both paths share
/// the exact same method signatures, so the pool code is target-agnostic.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait Transport: MaybeSendSync + 'static {
    /// Dispatch a JSON-RPC payload to `url`. Returns the raw response body.
    async fn post_json(&self, url: &str, body: Vec<u8>) -> Result<Vec<u8>, String>;

    /// Dispatch with extra request headers (used for per-endpoint auth such as
    /// `Header { .. }` or `Bearer { .. }`).
    ///
    /// The default implementation ignores headers and delegates to
    /// [`post_json`](Transport::post_json), so existing transports keep working;
    /// transports that support auth (like the bundled `ReqwestTransport`)
    /// override this.
    async fn post_json_with_headers(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        let _ = headers;
        self.post_json(url, body).await
    }
}

// ────────────────────────────────────────────────────────────────────
// Default: reqwest-based transport
// ────────────────────────────────────────────────────────────────────

#[cfg(feature = "reqwest-transport")]
pub use reqwest_transport::ReqwestTransport;

#[cfg(feature = "reqwest-transport")]
mod reqwest_transport {
    use std::time::Duration;

    use async_trait::async_trait;

    use super::Transport;

    /// `reqwest`-backed transport. One client per pool is the intended usage;
    /// the client already pools HTTP connections internally.
    #[derive(Debug, Clone)]
    pub struct ReqwestTransport {
        client: reqwest::Client,
    }

    impl ReqwestTransport {
        /// Fresh transport with a default 15-second timeout.
        pub fn new() -> Self {
            Self::with_timeout(Duration::from_secs(15))
        }

        pub fn with_timeout(timeout: Duration) -> Self {
            let client = reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .expect("reqwest client should build");
            Self { client }
        }

        pub fn from_client(client: reqwest::Client) -> Self {
            Self { client }
        }
    }

    impl Default for ReqwestTransport {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl Transport for ReqwestTransport {
        async fn post_json(&self, url: &str, body: Vec<u8>) -> Result<Vec<u8>, String> {
            self.post_json_with_headers(url, &[], body).await
        }

        async fn post_json_with_headers(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: Vec<u8>,
        ) -> Result<Vec<u8>, String> {
            let mut req = self
                .client
                .post(url)
                .header("Content-Type", "application/json")
                .body(body);
            for (name, value) in headers {
                req = req.header(name, value);
            }
            let response = req.send().await.map_err(|e| format!("send error: {e}"))?;
            let status = response.status();
            let bytes = response
                .bytes()
                .await
                .map_err(|e| format!("read error: {e}"))?;
            if !status.is_success() {
                let body = String::from_utf8_lossy(&bytes);
                return Err(format!("http {} {}", status.as_u16(), body));
            }
            Ok(bytes.to_vec())
        }
    }
}
