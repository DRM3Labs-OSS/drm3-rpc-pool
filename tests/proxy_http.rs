//! End-to-end tests for the HTTP JSON-RPC proxy daemon.
//!
//! Spins up mock upstream RPC servers (hyper) and the proxy router (axum) on
//! random loopback ports, then drives the proxy with a real HTTP client to
//! exercise failover, 429 detection, JSON-RPC error relay, batch requests, and
//! the `/health` + `/metrics` surfaces.

#![cfg(feature = "daemon")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use drm3_rpc_pool::{proxy, RpcEndpoint, RpcPool, RpcPoolConfig};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::TcpListener;

type HandlerFn = Arc<dyn Fn(Vec<u8>) -> (StatusCode, Bytes) + Send + Sync + 'static>;

/// Spawn a one-off hyper upstream and return its bound URL.
async fn spawn_upstream(handler: HandlerFn) -> String {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let io = TokioIo::new(stream);
            let handler = handler.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<Incoming>| {
                    let handler = handler.clone();
                    async move {
                        let body = req
                            .into_body()
                            .collect()
                            .await
                            .map(|c| c.to_bytes().to_vec())
                            .unwrap_or_default();
                        let (status, out) = handler(body);
                        Ok::<_, hyper::Error>(
                            Response::builder()
                                .status(status)
                                .header("content-type", "application/json")
                                .body(Full::new(out))
                                .unwrap(),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });
    format!("http://{addr}")
}

fn ok_result(result: Value) -> HandlerFn {
    Arc::new(move |_b| {
        (
            StatusCode::OK,
            Bytes::from(
                serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":result.clone()}))
                    .unwrap(),
            ),
        )
    })
}

fn rpc_error(code: i64, message: &'static str) -> HandlerFn {
    Arc::new(move |_b| {
        (
            StatusCode::OK,
            Bytes::from(
                serde_json::to_vec(
                    &json!({"jsonrpc":"2.0","id":1,"error":{"code":code,"message":message}}),
                )
                .unwrap(),
            ),
        )
    })
}

fn http_429() -> HandlerFn {
    Arc::new(|_b| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Bytes::from(b"{\"error\":\"rate limited\"}".to_vec()),
        )
    })
}

/// Start the proxy in front of `config` and return its base URL.
async fn spawn_proxy(config: RpcPoolConfig) -> String {
    let pool = RpcPool::from_config(config).unwrap();
    let app = proxy::build_router(pool);
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn config_from_urls(urls: &[String]) -> RpcPoolConfig {
    RpcPoolConfig {
        endpoints: urls
            .iter()
            .enumerate()
            .map(|(i, u)| RpcEndpoint {
                priority: i as u32,
                ..RpcEndpoint::new(u.clone())
            })
            .collect(),
        request_timeout_ms: Some(2000),
        ..RpcPoolConfig::default()
    }
}

async fn post_json(client: &reqwest::Client, url: &str, body: Value) -> Value {
    client
        .post(url)
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn proxy_forwards_single_request() {
    let up = spawn_upstream(ok_result(json!("0xabc"))).await;
    let proxy = spawn_proxy(config_from_urls(&[up])).await;
    let client = reqwest::Client::new();
    let resp = post_json(
        &client,
        &proxy,
        json!({"jsonrpc":"2.0","id":42,"method":"eth_blockNumber","params":[]}),
    )
    .await;
    assert_eq!(resp["result"], json!("0xabc"));
    // Client id is preserved.
    assert_eq!(resp["id"], json!(42));
}

#[tokio::test]
async fn proxy_fails_over_on_429() {
    let bad = spawn_upstream(http_429()).await;
    let good = spawn_upstream(ok_result(json!("0x1"))).await;
    let proxy = spawn_proxy(config_from_urls(&[bad, good])).await;
    let client = reqwest::Client::new();
    let resp = post_json(
        &client,
        &proxy,
        json!({"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}),
    )
    .await;
    assert_eq!(resp["result"], json!("0x1"));
}

#[tokio::test]
async fn proxy_relays_jsonrpc_error_without_failover() {
    // First upstream returns a revert; proxy must relay it, not fail over.
    let revert = spawn_upstream(rpc_error(3, "execution reverted")).await;
    let fallback = spawn_upstream(ok_result(json!("unreached"))).await;
    let proxy = spawn_proxy(config_from_urls(&[revert, fallback])).await;
    let client = reqwest::Client::new();
    let resp = post_json(
        &client,
        &proxy,
        json!({"jsonrpc":"2.0","id":7,"method":"eth_call","params":[]}),
    )
    .await;
    assert_eq!(resp["error"]["code"], json!(3));
    assert_eq!(resp["error"]["message"], json!("execution reverted"));
    assert_eq!(resp["id"], json!(7));
}

#[tokio::test]
async fn proxy_handles_batch_requests() {
    let up = spawn_upstream(ok_result(json!("0x5"))).await;
    let proxy = spawn_proxy(config_from_urls(&[up])).await;
    let client = reqwest::Client::new();
    let resp = post_json(
        &client,
        &proxy,
        json!([
            {"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]},
            {"jsonrpc":"2.0","id":2,"method":"eth_chainId","params":[]}
        ]),
    )
    .await;
    let arr = resp.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"], json!(1));
    assert_eq!(arr[1]["id"], json!(2));
    assert_eq!(arr[0]["result"], json!("0x5"));
}

#[tokio::test]
async fn proxy_all_upstreams_down_returns_error_envelope() {
    let a = spawn_upstream(http_429()).await;
    let b = spawn_upstream(http_429()).await;
    let proxy = spawn_proxy(config_from_urls(&[a, b])).await;
    let client = reqwest::Client::new();
    let resp = post_json(
        &client,
        &proxy,
        json!({"jsonrpc":"2.0","id":9,"method":"eth_blockNumber","params":[]}),
    )
    .await;
    assert!(resp["error"].is_object());
    assert_eq!(resp["id"], json!(9));
}

#[tokio::test]
async fn proxy_missing_method_is_invalid_request() {
    let up = spawn_upstream(ok_result(json!("x"))).await;
    let proxy = spawn_proxy(config_from_urls(&[up])).await;
    let client = reqwest::Client::new();
    let resp = post_json(&client, &proxy, json!({"jsonrpc":"2.0","id":1})).await;
    assert_eq!(resp["error"]["code"], json!(-32600));
}

#[tokio::test]
async fn health_and_metrics_endpoints() {
    let up = spawn_upstream(ok_result(json!("0x1"))).await;
    let proxy = spawn_proxy(config_from_urls(&[up])).await;
    let client = reqwest::Client::new();

    let health: Value = client
        .get(format!("{proxy}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["status"], json!("ok"));
    assert_eq!(health["endpoints_total"], json!(1));

    let metrics: Value = client
        .get(format!("{proxy}/metrics"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(metrics["endpoints"].is_array());
}

#[tokio::test]
async fn auth_bearer_reaches_upstream_over_http() {
    // Custom hyper upstream that records the Authorization header it received.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let seen_clone = seen.clone();
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let io = TokioIo::new(stream);
            let seen = seen_clone.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<Incoming>| {
                    let seen = seen.clone();
                    async move {
                        if let Some(v) = req.headers().get("authorization") {
                            *seen.lock().unwrap() = Some(v.to_str().unwrap_or("").to_string());
                        }
                        let _ = req.into_body().collect().await;
                        Ok::<_, hyper::Error>(
                            Response::builder()
                                .status(StatusCode::OK)
                                .header("content-type", "application/json")
                                .body(Full::new(Bytes::from(
                                    serde_json::to_vec(
                                        &json!({"jsonrpc":"2.0","id":1,"result":"0x1"}),
                                    )
                                    .unwrap(),
                                )))
                                .unwrap(),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });
    let url = format!("http://{addr}");

    let cfg = RpcPoolConfig {
        endpoints: vec![
            RpcEndpoint::new(url).with_auth(drm3_rpc_pool::Auth::Bearer {
                token: "tok-xyz".into(),
            }),
        ],
        ..RpcPoolConfig::default()
    };
    let proxy = spawn_proxy(cfg).await;
    let client = reqwest::Client::new();
    let resp = post_json(
        &client,
        &proxy,
        json!({"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}),
    )
    .await;
    assert_eq!(resp["result"], json!("0x1"));
    assert_eq!(
        seen.lock().unwrap().clone(),
        Some("Bearer tok-xyz".to_string())
    );
}

#[tokio::test]
async fn proxy_respects_request_timeout_failover() {
    // Slow upstream exceeds the 2s config timeout? Keep it simple: a hanging
    // upstream forces failover to the fast one within the configured timeout.
    let slow: HandlerFn = Arc::new(|_b| {
        std::thread::sleep(Duration::from_millis(500));
        (
            StatusCode::OK,
            Bytes::from(
                serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":"late"})).unwrap(),
            ),
        )
    });
    let slow_url = spawn_upstream(slow).await;
    let fast_url = spawn_upstream(ok_result(json!("fast"))).await;
    let mut cfg = config_from_urls(&[slow_url, fast_url]);
    cfg.request_timeout_ms = Some(100);
    let proxy = spawn_proxy(cfg).await;
    let client = reqwest::Client::new();
    let resp = post_json(
        &client,
        &proxy,
        json!({"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}),
    )
    .await;
    assert_eq!(resp["result"], json!("fast"));
}
