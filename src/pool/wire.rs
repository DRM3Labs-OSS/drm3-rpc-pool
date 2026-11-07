//! JSON-RPC wire shapes and response parsing for the pool.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(super) struct JsonRpcRequest<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    pub params: Value,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    #[allow(dead_code)]
    id: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcErrorBody>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcErrorBody {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ResponseError {
    #[error("invalid json: {0}")]
    InvalidJson(String),
    #[error("no result in response")]
    EmptyResult,
}

/// A JSON-RPC error envelope relayed verbatim from an upstream. Distinct from a
/// transport failure: a revert or "method not found" is a *valid* answer that
/// the proxy must pass through rather than fail over on.
#[derive(Debug, Clone)]
pub(super) struct UpstreamRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

/// Outcome of forwarding a request to a single upstream, before failover logic.
#[derive(Debug)]
pub(super) enum RawOutcome {
    /// A successful `result`.
    Result(Value),
    /// A well-formed JSON-RPC error envelope (relay, do not fail over).
    RpcError(UpstreamRpcError),
}

/// Classify a response body. Used by both [`RpcPool::call`] and
/// [`RpcPool::forward`]: only invalid JSON triggers failover; both `result`
/// and `error` envelopes are a usable upstream answer. A present-but-null
/// `result` surfaces as [`ResponseError::EmptyResult`], which callers treat as
/// a valid null answer, not a failure.
///
/// [`RpcPool::call`]: super::RpcPool::call
/// [`RpcPool::forward`]: super::RpcPool::forward
pub(super) fn classify_response(bytes: &[u8]) -> Result<RawOutcome, ResponseError> {
    let parsed: JsonRpcResponse =
        serde_json::from_slice(bytes).map_err(|e| ResponseError::InvalidJson(e.to_string()))?;
    if let Some(err) = parsed.error {
        return Ok(RawOutcome::RpcError(UpstreamRpcError {
            code: err.code,
            message: err.message,
            data: err.data,
        }));
    }
    match parsed.result {
        Some(v) => Ok(RawOutcome::Result(v)),
        None => Err(ResponseError::EmptyResult),
    }
}
