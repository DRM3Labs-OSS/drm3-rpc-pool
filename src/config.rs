//! Configuration types for the RPC pool.

use serde::{Deserialize, Serialize};

use crate::error::RpcError;

/// Per-endpoint authentication. Every string value supports `${ENV_VAR}`
/// templating so secrets stay in the environment, never in the config file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    /// No auth. The endpoint URL is used as-is.
    #[default]
    None,
    /// The API key is already baked into the URL (e.g. Alchemy/Infura style
    /// `https://.../v2/${ALCHEMY_KEY}`). No extra headers are added; this is a
    /// declarative marker that the URL itself carries the secret.
    UrlKey,
    /// A custom request header, e.g. `{ name = "X-API-Key", value = "${KEY}" }`.
    Header { name: String, value: String },
    /// `Authorization: Bearer <token>`.
    Bearer { token: String },
}

impl Auth {
    /// Resolve into the concrete request headers this auth mode contributes.
    /// `UrlKey`/`None` add no headers.
    pub fn headers(&self) -> Vec<(String, String)> {
        match self {
            Auth::None | Auth::UrlKey => Vec::new(),
            Auth::Header { name, value } => vec![(name.clone(), value.clone())],
            Auth::Bearer { token } => vec![("Authorization".into(), format!("Bearer {token}"))],
        }
    }

    /// Expand `${ENV_VAR}` references inside every string field.
    fn resolve_env(&mut self) -> Result<(), RpcError> {
        match self {
            Auth::None | Auth::UrlKey => {}
            Auth::Header { name, value } => {
                *name = expand_env(name)?;
                *value = expand_env(value)?;
            }
            Auth::Bearer { token } => {
                *token = expand_env(token)?;
            }
        }
        Ok(())
    }
}

/// Expand every `${VAR}` occurrence in `input` from the process environment.
/// Unset variables are a hard error so a missing key never silently degrades to
/// an unauthenticated request.
pub fn expand_env(input: &str) -> Result<String, RpcError> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(RpcError::Config(format!(
                "unterminated ${{...}} in config value: {input:?}"
            )));
        };
        let var = &after[..end];
        let val = std::env::var(var).map_err(|_| {
            RpcError::Config(format!(
                "environment variable {var} referenced in config is unset"
            ))
        })?;
        out.push_str(&val);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A single JSON-RPC endpoint.
///
/// `priority` is the sort key. Lower values are tried first. Ties are broken
/// by insertion order in the `endpoints` vector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RpcEndpoint {
    /// HTTP(S) URL for this endpoint. Env-var templating is the caller's
    /// responsibility - the pool treats this as a literal URL.
    pub url: String,

    /// Human-readable label (e.g. `"alchemy-mainnet"`, `"llamarpc"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Sort key. Lower is tried first. Defaults to 0 on deserialize.
    #[serde(default)]
    pub priority: u32,

    /// Methods (or method families) this endpoint is known to support.
    /// Empty list = "assume it supports everything". Non-empty list = strict
    /// routing: only calls whose capability appears here will dispatch.
    #[serde(default)]
    pub capabilities: Vec<RpcCapability>,

    /// Client-side throttle in requests per second. `None` = unthrottled.
    /// Enforced by the pool's per-endpoint governor when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rps: Option<u32>,

    /// Soft concurrency cap: once this many requests are in flight at this
    /// endpoint, new calls prefer a less-loaded peer (or the next priority
    /// tier) instead of piling on, and only fall back here if nothing else is
    /// available. `None` = uncapped. This is what turns a ranked pool adaptive:
    /// a primary carries load up to its cap, then the burst spills to failover
    /// endpoints rather than saturating the primary and stalling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_in_flight: Option<u32>,

    /// Per-endpoint authentication. Defaults to `Auth::None`.
    #[serde(default, skip_serializing_if = "is_default_auth")]
    pub auth: Auth,
}

fn is_default_auth(auth: &Auth) -> bool {
    matches!(auth, Auth::None)
}

impl RpcEndpoint {
    /// Construct a new endpoint with all-capable routing (empty `capabilities`).
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            label: None,
            priority: 0,
            capabilities: Vec::new(),
            max_rps: None,
            max_in_flight: None,
            auth: Auth::None,
        }
    }

    /// Builder: set the authentication mode.
    pub fn with_auth(mut self, auth: Auth) -> Self {
        self.auth = auth;
        self
    }

    /// Expand `${ENV_VAR}` references in the URL and auth fields.
    pub fn resolve_env(&mut self) -> Result<(), RpcError> {
        self.url = expand_env(&self.url)?;
        self.auth.resolve_env()?;
        Ok(())
    }

    /// Short tag for logs and metrics - label if set, else URL.
    pub fn tag(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.url)
    }

    /// Whether this endpoint can serve a given capability.
    ///
    /// An empty capability list is treated as "supports everything" - this
    /// matches operator intuition for minimally-configured endpoints.
    pub fn supports(&self, capability: &RpcCapability) -> bool {
        if self.capabilities.is_empty() {
            return true;
        }
        self.capabilities.iter().any(|c| c == capability)
    }
}

/// Default proxy listen address used when the config omits `listen`.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8545";

fn default_listen() -> String {
    DEFAULT_LISTEN.to_string()
}

/// Default per-request retry budget (number of endpoints to try beyond the
/// first before giving up - `0` means "try every candidate", which is the
/// pool's natural first-success-wins behaviour).
const fn default_max_retries() -> u32 {
    0
}

/// Unbounded pool configuration. Order matters only insofar as `priority` ties
/// are broken by it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcPoolConfig {
    /// Address the proxy daemon binds to (`host:port`). Ignored by library
    /// callers; only the `drm3-rpc-pool` binary reads it. Env-templatable.
    #[serde(default = "default_listen")]
    pub listen: String,

    /// Per-request timeout in milliseconds applied by the transport. `None`
    /// uses the transport default (15s for the bundled reqwest transport).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_ms: Option<u64>,

    /// Maximum number of endpoints to attempt per call. `0` (default) = try
    /// every healthy, capable candidate (classic failover).
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    pub endpoints: Vec<RpcEndpoint>,
}

impl Default for RpcPoolConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            request_timeout_ms: None,
            max_retries: default_max_retries(),
            endpoints: Vec::new(),
        }
    }
}

impl RpcPoolConfig {
    /// Quick constructor from a list of plain URLs. Endpoints get
    /// `priority = index` (the list is a ranked failover order, first
    /// preferred), no label, no capability list. Give two or more endpoints
    /// the *same* `priority` to make them peers - the pool then spreads
    /// concurrent load across that tier by least in-flight instead of always
    /// hitting the first.
    pub fn from_urls<I, S>(urls: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let endpoints = urls
            .into_iter()
            .enumerate()
            .map(|(i, url)| RpcEndpoint {
                url: url.into(),
                label: None,
                priority: i as u32,
                capabilities: Vec::new(),
                max_rps: None,
                max_in_flight: None,
                auth: Auth::None,
            })
            .collect();
        Self {
            endpoints,
            ..Self::default()
        }
    }

    /// Parse a TOML config string. Expands `${ENV_VAR}` templating in the
    /// `listen` address, every endpoint URL, and every auth field, then
    /// validates.
    pub fn from_toml_str(s: &str) -> Result<Self, RpcError> {
        let mut cfg: RpcPoolConfig =
            toml::from_str(s).map_err(|e| RpcError::Config(format!("TOML parse error: {e}")))?;
        cfg.resolve_env()?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load and parse a TOML config file.
    pub fn from_toml_file(path: impl AsRef<std::path::Path>) -> Result<Self, RpcError> {
        let path = path.as_ref();
        let s = std::fs::read_to_string(path)
            .map_err(|e| RpcError::Config(format!("cannot read config {}: {e}", path.display())))?;
        Self::from_toml_str(&s)
    }

    /// Serialize back to a TOML string (secrets shown as their resolved values
    /// if already expanded - prefer building from un-resolved sources for
    /// round-trips).
    pub fn to_toml_string(&self) -> Result<String, RpcError> {
        toml::to_string_pretty(self)
            .map_err(|e| RpcError::Config(format!("TOML serialize error: {e}")))
    }

    /// Expand `${ENV_VAR}` references across `listen` and all endpoints.
    pub fn resolve_env(&mut self) -> Result<(), RpcError> {
        self.listen = expand_env(&self.listen)?;
        for ep in &mut self.endpoints {
            ep.resolve_env()?;
        }
        Ok(())
    }

    /// Reject empty or duplicate-URL configs.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.endpoints.is_empty() {
            return Err(RpcError::Config("endpoint list is empty".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for ep in &self.endpoints {
            if ep.url.trim().is_empty() {
                return Err(RpcError::Config("endpoint URL is empty".into()));
            }
            if !seen.insert(ep.url.clone()) {
                return Err(RpcError::Config(format!(
                    "duplicate endpoint URL: {}",
                    ep.url
                )));
            }
        }
        Ok(())
    }
}

/// Capabilities an endpoint may declare. Matched against method names inside
/// the pool.
///
/// Not every JSON-RPC method maps to a dedicated variant. The common methods
/// are enumerated; everything else falls into `Other`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RpcCapability {
    EthCall,
    EthGetLogs,
    EthBlockNumber,
    EthChainId,
    EthGetBalance,
    EthGetTransactionReceipt,
    EthGetTransactionCount,
    EthSendRawTransaction,
    EthEstimateGas,
    EthGasPrice,
    /// WebSocket/pub-sub subscription (implementation-defined).
    Subscribe,
    /// Anything else. Match by the wire method name, e.g. `"debug_traceCall"`.
    Other(String),
}

impl RpcCapability {
    /// Derive the capability for a given JSON-RPC method name. Unknown methods
    /// become `Other(method.to_string())`.
    pub fn for_method(method: &str) -> Self {
        match method {
            "eth_call" => RpcCapability::EthCall,
            "eth_getLogs" => RpcCapability::EthGetLogs,
            "eth_blockNumber" => RpcCapability::EthBlockNumber,
            "eth_chainId" => RpcCapability::EthChainId,
            "eth_getBalance" => RpcCapability::EthGetBalance,
            "eth_getTransactionReceipt" => RpcCapability::EthGetTransactionReceipt,
            "eth_getTransactionCount" => RpcCapability::EthGetTransactionCount,
            "eth_sendRawTransaction" => RpcCapability::EthSendRawTransaction,
            "eth_estimateGas" => RpcCapability::EthEstimateGas,
            "eth_gasPrice" => RpcCapability::EthGasPrice,
            "eth_subscribe" | "eth_unsubscribe" => RpcCapability::Subscribe,
            other => RpcCapability::Other(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_capabilities_supports_anything() {
        let ep = RpcEndpoint::new("https://rpc");
        assert!(ep.supports(&RpcCapability::EthGetLogs));
        assert!(ep.supports(&RpcCapability::Other("debug_traceCall".into())));
    }

    #[test]
    fn explicit_capabilities_exclude_others() {
        let ep = RpcEndpoint {
            capabilities: vec![RpcCapability::EthCall],
            ..RpcEndpoint::new("https://rpc")
        };
        assert!(ep.supports(&RpcCapability::EthCall));
        assert!(!ep.supports(&RpcCapability::EthGetLogs));
    }

    #[test]
    fn validate_rejects_empty_pool() {
        let cfg = RpcPoolConfig::default();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_duplicate_urls() {
        let cfg = RpcPoolConfig::from_urls(["https://rpc.a", "https://rpc.a"]);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_accepts_priority_ties() {
        let cfg = RpcPoolConfig {
            endpoints: vec![
                RpcEndpoint {
                    url: "https://a".into(),
                    priority: 5,
                    ..RpcEndpoint::new("https://a")
                },
                RpcEndpoint {
                    url: "https://b".into(),
                    priority: 5,
                    ..RpcEndpoint::new("https://b")
                },
            ],
            ..RpcPoolConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn capability_for_known_methods() {
        assert_eq!(
            RpcCapability::for_method("eth_call"),
            RpcCapability::EthCall
        );
        assert_eq!(
            RpcCapability::for_method("eth_getLogs"),
            RpcCapability::EthGetLogs
        );
        assert_eq!(
            RpcCapability::for_method("eth_subscribe"),
            RpcCapability::Subscribe
        );
    }

    #[test]
    fn capability_for_unknown_method() {
        let cap = RpcCapability::for_method("debug_traceCall");
        assert_eq!(cap, RpcCapability::Other("debug_traceCall".into()));
    }

    #[test]
    fn tag_falls_back_to_url() {
        let mut ep = RpcEndpoint::new("https://rpc");
        assert_eq!(ep.tag(), "https://rpc");
        ep.label = Some("cool".into());
        assert_eq!(ep.tag(), "cool");
    }

    #[test]
    fn expand_env_replaces_known_var() {
        std::env::set_var("DRM3_TEST_KEY_A", "secret123");
        let out = expand_env("https://x/v2/${DRM3_TEST_KEY_A}").unwrap();
        assert_eq!(out, "https://x/v2/secret123");
    }

    #[test]
    fn expand_env_errors_on_unset_var() {
        let err = expand_env("${DRM3_DEFINITELY_UNSET_XYZ}").unwrap_err();
        assert!(matches!(err, RpcError::Config(_)));
    }

    #[test]
    fn expand_env_handles_multiple_and_no_vars() {
        std::env::set_var("DRM3_TEST_A", "aa");
        std::env::set_var("DRM3_TEST_B", "bb");
        assert_eq!(expand_env("plain").unwrap(), "plain");
        assert_eq!(
            expand_env("${DRM3_TEST_A}-${DRM3_TEST_B}-x").unwrap(),
            "aa-bb-x"
        );
    }

    #[test]
    fn auth_header_and_bearer_resolve() {
        assert!(Auth::None.headers().is_empty());
        assert!(Auth::UrlKey.headers().is_empty());
        let h = Auth::Header {
            name: "X-API-Key".into(),
            value: "k".into(),
        };
        assert_eq!(h.headers(), vec![("X-API-Key".into(), "k".into())]);
        let b = Auth::Bearer { token: "t".into() };
        assert_eq!(
            b.headers(),
            vec![("Authorization".into(), "Bearer t".into())]
        );
    }

    #[test]
    fn from_toml_str_parses_and_templates() {
        std::env::set_var("DRM3_TEST_ALCHEMY", "abc");
        std::env::set_var("DRM3_TEST_HEADER", "hval");
        let toml = r#"
            listen = "0.0.0.0:9000"
            request_timeout_ms = 5000
            max_retries = 3

            [[endpoints]]
            url = "https://eth.example/v2/${DRM3_TEST_ALCHEMY}"
            label = "alchemy"
            priority = 0
            auth = { type = "url_key" }

            [[endpoints]]
            url = "https://other.example"
            priority = 1
            capabilities = ["eth_call"]
            max_rps = 25
            auth = { type = "header", name = "X-Key", value = "${DRM3_TEST_HEADER}" }
        "#;
        let cfg = RpcPoolConfig::from_toml_str(toml).unwrap();
        assert_eq!(cfg.listen, "0.0.0.0:9000");
        assert_eq!(cfg.request_timeout_ms, Some(5000));
        assert_eq!(cfg.max_retries, 3);
        assert_eq!(cfg.endpoints.len(), 2);
        assert_eq!(cfg.endpoints[0].url, "https://eth.example/v2/abc");
        assert_eq!(cfg.endpoints[0].auth, Auth::UrlKey);
        assert_eq!(cfg.endpoints[1].max_rps, Some(25));
        assert_eq!(
            cfg.endpoints[1].auth,
            Auth::Header {
                name: "X-Key".into(),
                value: "hval".into()
            }
        );
    }

    #[test]
    fn toml_round_trip_preserves_endpoints() {
        let cfg = RpcPoolConfig {
            listen: "127.0.0.1:1234".into(),
            max_retries: 2,
            request_timeout_ms: Some(7000),
            endpoints: vec![
                RpcEndpoint {
                    label: Some("a".into()),
                    capabilities: vec![RpcCapability::EthCall],
                    max_rps: Some(10),
                    auth: Auth::Bearer {
                        token: "tok".into(),
                    },
                    ..RpcEndpoint::new("https://a")
                },
                RpcEndpoint::new("https://b"),
            ],
        };
        let s = cfg.to_toml_string().unwrap();
        // Parse back without env resolution (no ${} present) via raw toml.
        let back: RpcPoolConfig = toml::from_str(&s).unwrap();
        assert_eq!(back.listen, "127.0.0.1:1234");
        assert_eq!(back.max_retries, 2);
        assert_eq!(back.request_timeout_ms, Some(7000));
        assert_eq!(back.endpoints.len(), 2);
        assert_eq!(back.endpoints[0].auth, cfg.endpoints[0].auth);
        assert_eq!(back.endpoints[1].auth, Auth::None);
    }

    #[test]
    fn from_toml_str_rejects_empty_endpoints() {
        let err = RpcPoolConfig::from_toml_str("listen = \"x:1\"\nendpoints = []\n").unwrap_err();
        assert!(matches!(err, RpcError::Config(_)));
    }
}
