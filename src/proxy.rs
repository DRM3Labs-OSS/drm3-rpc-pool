//! The HTTP JSON-RPC proxy surface.
//!
//! Builds an axum [`Router`] that forwards every incoming JSON-RPC request
//! (single object or batch array) through an [`RpcPool`], plus `/health` and
//! `/metrics` endpoints. The `drm3-rpc-pool` binary wraps this; it is exposed
//! from the library (behind the `daemon` feature) so it can be driven directly
//! and tested end-to-end.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{ForwardResult, RpcError, RpcPool};

/// Shared handler state.
#[derive(Clone)]
pub struct AppState {
    pub pool: RpcPool,
}

/// Build the proxy router: `POST /` (JSON-RPC), `GET /health`, `GET /metrics`.
pub fn build_router(pool: RpcPool) -> Router {
    Router::new()
        .route("/", post(rpc_handler))
        .route("/health", get(health_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(AppState { pool })
}

/// JSON-RPC entrypoint. Accepts a single request object or a batch array.
async fn rpc_handler(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    match payload {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(handle_one(&state.pool, item).await);
            }
            (StatusCode::OK, Json(Value::Array(out)))
        }
        single => (StatusCode::OK, Json(handle_one(&state.pool, single).await)),
    }
}

/// Proxy a single JSON-RPC request value and build its response envelope,
/// preserving the client's `id`.
pub async fn handle_one(pool: &RpcPool, req: Value) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = match req.get("method").and_then(Value::as_str) {
        Some(m) => m.to_string(),
        None => return rpc_error(id, -32600, "Invalid Request: missing method", None),
    };
    let params = req.get("params").cloned().unwrap_or_else(|| json!([]));

    match pool.forward(&method, params).await {
        Ok(ForwardResult::Result(result)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
        Ok(ForwardResult::Error {
            code,
            message,
            data,
        }) => rpc_error(id, code, &message, data),
        Err(e) => map_pool_error(id, &e),
    }
}

fn map_pool_error(id: Value, err: &RpcError) -> Value {
    let (code, message) = match err {
        RpcError::NoCandidates { .. } => (-32011, format!("no healthy endpoints: {err}")),
        RpcError::AllFailed { .. } => (-32010, format!("all upstreams failed: {err}")),
        other => (-32603, format!("proxy error: {other}")),
    };
    rpc_error(id, code, &message, None)
}

fn rpc_error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message });
    if let Some(d) = data {
        error["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

/// Liveness/readiness: 200 if the pool has any endpoints.
async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let statuses = state.pool.status();
    let total = statuses.len();
    let healthy = statuses
        .iter()
        .filter(|s| s.health.consecutive_failures == 0)
        .count();
    let ok = total > 0;
    let body = json!({
        "status": if ok { "ok" } else { "empty" },
        "endpoints_total": total,
        "endpoints_healthy": healthy,
    });
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body))
}

/// Per-endpoint metrics snapshot (JSON).
async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({ "endpoints": state.pool.status() })),
    )
}
