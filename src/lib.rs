//! drm3-rpc-pool - resilient JSON-RPC failover pool for any EVM chain.
//!
//! Works with any EVM JSON-RPC endpoints (Ethereum, Base, Arbitrum, Optimism,
//! Polygon, BNB, and so on). Every capability is a generic `eth_*` method;
//! there is no chain-specific code. Tag each endpoint with the methods you
//! intend to send there and the pool routes around incapable or unhealthy
//! providers.
//!
//! # Core model
//!
//! - An ordered, unbounded pool of endpoints (`RpcPoolConfig`).
//! - First-success-wins dispatch in priority order.
//! - Per-endpoint health state, exponential backoff, automatic demotion.
//! - Capability-based routing (skip endpoints that do not support a method).
//! - Metrics hook for request count, latency, error rate.
//! - Transport trait so any HTTP client can drive the pool. A `reqwest`-based
//!   default implementation ships behind the `reqwest-transport` feature.
//!
//! See the README for a usage example.

#![warn(clippy::all)]
#![deny(rust_2018_idioms)]

pub mod config;
pub mod error;
pub mod health;
pub mod metrics;
pub mod pool;
pub mod presets;
#[cfg(feature = "daemon")]
pub mod proxy;
pub mod send_sync;
pub mod transport;

pub use config::{expand_env, Auth, RpcCapability, RpcEndpoint, RpcPoolConfig, DEFAULT_LISTEN};
pub use error::RpcError;
pub use health::{BackoffPolicy, EndpointHealth, EndpointStatus};
pub use metrics::{Metrics, NoopMetrics};
pub use pool::{ForwardResult, PoolEntryStatus, RpcPool};
pub use transport::Transport;

#[cfg(feature = "reqwest-transport")]
pub use transport::ReqwestTransport;
