//! A [`Transport`] backed by the platform `fetch`.
//!
//! Works in the browser (Window), in Web Workers, and in Node 18+ (which exposes
//! a global `fetch`). We resolve `fetch` off `globalThis` rather than binding to
//! `Window` specifically so the same wasm runs in every context.
//!
//! The browser `fetch` future holds `JsValue`s and is therefore not `Send`,
//! which is why the core `Transport` trait relaxes its bounds on wasm and uses
//! `async_trait(?Send)`.

use async_trait::async_trait;
use drm3_rpc_pool::Transport;
use js_sys::{Promise, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};

/// Transport that POSTs JSON via the global `fetch`.
#[derive(Default)]
pub struct FetchTransport;

impl FetchTransport {
    pub fn new() -> Self {
        Self
    }
}

fn js_err_to_string(prefix: &str, e: JsValue) -> String {
    format!(
        "{prefix}: {}",
        e.as_string()
            .or_else(|| js_sys::JSON::stringify(&e).ok().and_then(|s| s.as_string()))
            .unwrap_or_else(|| "<non-string JS error>".to_string())
    )
}

/// Call `globalThis.fetch(request)` regardless of Window/Worker/Node context.
fn global_fetch(request: &Request) -> Result<Promise, String> {
    let global = js_sys::global();
    let fetch_fn = Reflect::get(&global, &JsValue::from_str("fetch"))
        .map_err(|e| js_err_to_string("no global fetch", e))?;
    let fetch_fn: js_sys::Function = fetch_fn
        .dyn_into()
        .map_err(|_| "global `fetch` is not callable".to_string())?;
    let result = fetch_fn
        .call1(&global, request.as_ref())
        .map_err(|e| js_err_to_string("fetch call failed", e))?;
    result
        .dyn_into::<Promise>()
        .map_err(|_| "fetch did not return a Promise".to_string())
}

#[async_trait(?Send)]
impl Transport for FetchTransport {
    async fn post_json(&self, url: &str, body: Vec<u8>) -> Result<Vec<u8>, String> {
        self.post_json_with_headers(url, &[], body).await
    }

    async fn post_json_with_headers(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        let opts = RequestInit::new();
        opts.set_method("POST");

        // Body as a Uint8Array (avoids UTF-8 round-trips and works for binary).
        let body_array = Uint8Array::from(body.as_slice());
        opts.set_body(&body_array);

        let request = Request::new_with_str_and_init(url, &opts)
            .map_err(|e| js_err_to_string("request build failed", e))?;

        let req_headers = request.headers();
        req_headers
            .set("Content-Type", "application/json")
            .map_err(|e| js_err_to_string("header set failed", e))?;
        for (name, value) in headers {
            req_headers
                .set(name, value)
                .map_err(|e| js_err_to_string("auth header set failed", e))?;
        }

        let promise = global_fetch(&request)?;
        let resp_value = JsFuture::from(promise)
            .await
            .map_err(|e| js_err_to_string("fetch rejected", e))?;
        let response: Response = resp_value
            .dyn_into()
            .map_err(|_| "fetch result was not a Response".to_string())?;

        let status = response.status();

        let buf_promise = response
            .array_buffer()
            .map_err(|e| js_err_to_string("array_buffer() failed", e))?;
        let buf_value = JsFuture::from(buf_promise)
            .await
            .map_err(|e| js_err_to_string("reading body failed", e))?;
        let bytes = Uint8Array::new(&buf_value).to_vec();

        if !(200..300).contains(&status) {
            let snippet = String::from_utf8_lossy(&bytes);
            return Err(format!("http {status} {snippet}"));
        }
        Ok(bytes)
    }
}
