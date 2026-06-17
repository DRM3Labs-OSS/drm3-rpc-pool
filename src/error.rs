//! Error type for drm3-rpc-pool.

use thiserror::Error;

/// Error returned by the RPC pool.
#[derive(Debug, Error)]
pub enum RpcError {
    /// The pool had no candidates for the requested method. Either the pool is
    /// empty, no endpoint declared the capability, or every capable endpoint
    /// was in cooldown.
    #[error("no healthy endpoints support method {method}: {reason}")]
    NoCandidates { method: String, reason: String },

    /// Every candidate endpoint returned an error. The `attempts` list preserves
    /// the (endpoint_url, error_message) pairs so operators can see exactly what
    /// happened.
    #[error("all {count} RPC endpoints failed for {method}")]
    AllFailed {
        method: String,
        count: usize,
        attempts: Vec<(String, String)>,
    },

    /// Transport-layer failure (I/O, timeout, DNS). Wraps whatever the
    /// underlying `Transport` implementation returned.
    #[error("transport error for {endpoint}: {message}")]
    Transport { endpoint: String, message: String },

    /// The JSON-RPC server returned an error envelope (`error` field populated).
    #[error("rpc error from {endpoint}: {code} {message}")]
    JsonRpc {
        endpoint: String,
        code: i64,
        message: String,
    },

    /// Response body failed to parse as JSON-RPC.
    #[error("malformed response from {endpoint}: {message}")]
    Malformed { endpoint: String, message: String },

    /// Caller supplied an invalid configuration.
    #[error("invalid configuration: {0}")]
    Config(String),
}

impl RpcError {
    /// Convenience: treat an unknown error string as a transport failure for a
    /// specific endpoint.
    pub fn transport(endpoint: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Transport {
            endpoint: endpoint.into(),
            message: message.into(),
        }
    }
}
