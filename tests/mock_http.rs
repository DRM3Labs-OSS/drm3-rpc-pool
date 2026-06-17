//! Integration tests against a real in-process HTTP server.
//!
//! Boots a minimal `hyper` server per test on a random loopback port, wires the
//! pool to the built-in `ReqwestTransport`, and exercises rate-limit, timeout,
//! and 5xx response paths end-to-end.

#![cfg(feature = "reqwest-transport")]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use drm3_rpc_pool::{
    health::BackoffPolicy, NoopMetrics, ReqwestTransport, RpcError, RpcPool, RpcPoolConfig,
};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::json;
use tokio::net::TcpListener;

/// Handler signature: takes request bytes, returns (status, body).
type HandlerFn = Arc<dyn Fn(Vec<u8>) -> (StatusCode, Bytes) + Send + Sync + 'static>;

/// Spawn a one-off hyper server and return its bound URL.
async fn spawn_server(handler: HandlerFn) -> String {
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
                        let body_bytes = req
                            .into_body()
                            .collect()
                            .await
                            .map(|c| c.to_bytes().to_vec())
                            .unwrap_or_default();
                        let (status, body) = handler(body_bytes);
                        Ok::<_, hyper::Error>(
                            Response::builder()
                                .status(status)
                                .header("content-type", "application/json")
                                .body(Full::new(body))
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

fn always_200(result: serde_json::Value) -> HandlerFn {
    Arc::new(move |_body| {
        (
            StatusCode::OK,
            Bytes::from(
                serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": result.clone(),
                }))
                .unwrap(),
            ),
        )
    })
}

fn always_429() -> HandlerFn {
    Arc::new(|_body| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Bytes::from(b"{\"error\":\"rate limited\"}".to_vec()),
        )
    })
}

fn always_500() -> HandlerFn {
    Arc::new(|_body| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from(b"boom".to_vec()),
        )
    })
}

#[tokio::test]
async fn end_to_end_success_against_real_server() {
    let url = spawn_server(always_200(json!("0xdeadbeef"))).await;
    let pool = RpcPool::new(
        RpcPoolConfig::from_urls([url]),
        Arc::new(ReqwestTransport::with_timeout(Duration::from_secs(5))),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let result = pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(result, json!("0xdeadbeef"));
}

#[tokio::test]
async fn rate_limit_response_fails_over_to_next_endpoint() {
    let rate_limited = spawn_server(always_429()).await;
    let healthy = spawn_server(always_200(json!("0x1"))).await;
    let pool = RpcPool::new(
        RpcPoolConfig::from_urls([rate_limited, healthy]),
        Arc::new(ReqwestTransport::with_timeout(Duration::from_secs(5))),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let result = pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(result, json!("0x1"));
}

#[tokio::test]
async fn server_5xx_counts_as_failure_and_fails_over() {
    let broken = spawn_server(always_500()).await;
    let healthy = spawn_server(always_200(json!("0x99"))).await;
    let pool = RpcPool::new(
        RpcPoolConfig::from_urls([broken, healthy]),
        Arc::new(ReqwestTransport::with_timeout(Duration::from_secs(5))),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let result = pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(result, json!("0x99"));
}

#[tokio::test]
async fn all_endpoints_broken_reports_all_failed_with_details() {
    let a = spawn_server(always_500()).await;
    let b = spawn_server(always_429()).await;
    let pool = RpcPool::new(
        RpcPoolConfig::from_urls([a.clone(), b.clone()]),
        Arc::new(ReqwestTransport::with_timeout(Duration::from_secs(5))),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let err = pool.call("eth_blockNumber", json!([])).await.unwrap_err();
    match err {
        RpcError::AllFailed { attempts, .. } => {
            assert_eq!(attempts.len(), 2);
            let urls: Vec<_> = attempts.iter().map(|a| a.0.clone()).collect();
            assert!(urls.contains(&a));
            assert!(urls.contains(&b));
        }
        other => panic!("expected AllFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn timeout_is_treated_as_failure() {
    // Handler that deliberately hangs longer than the transport timeout.
    let slow: HandlerFn = Arc::new(|_body| {
        std::thread::sleep(Duration::from_millis(300));
        (
            StatusCode::OK,
            Bytes::from(
                serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": "too-late",
                }))
                .unwrap(),
            ),
        )
    });
    let slow_url = spawn_server(slow).await;
    let fast_url = spawn_server(always_200(json!("on-time"))).await;
    let pool = RpcPool::new(
        RpcPoolConfig::from_urls([slow_url, fast_url]),
        Arc::new(ReqwestTransport::with_timeout(Duration::from_millis(50))),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let result = pool.call("eth_blockNumber", json!([])).await.unwrap();
    assert_eq!(result, json!("on-time"));
}

#[tokio::test]
async fn concurrent_requests_across_real_server_preserve_counts() {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let handler: HandlerFn = Arc::new(move |_body| {
        counter_clone.fetch_add(1, Ordering::Relaxed);
        (
            StatusCode::OK,
            Bytes::from(
                serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": "0x1",
                }))
                .unwrap(),
            ),
        )
    });
    let url = spawn_server(handler).await;
    let pool = RpcPool::new(
        RpcPoolConfig::from_urls([url]),
        Arc::new(ReqwestTransport::with_timeout(Duration::from_secs(5))),
        Arc::new(NoopMetrics),
        BackoffPolicy::tight(),
    )
    .unwrap();
    let mut joins = Vec::new();
    for _ in 0..64 {
        let p = pool.clone();
        joins.push(tokio::spawn(async move {
            p.call("eth_blockNumber", json!([])).await.unwrap();
        }));
    }
    for j in joins {
        j.await.unwrap();
    }
    assert_eq!(counter.load(Ordering::Relaxed), 64);
}
