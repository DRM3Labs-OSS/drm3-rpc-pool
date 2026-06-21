//! The crate docs are the project README, so docs.rs and an agent reading the
//! source get the same usage guide. Source of truth: `../README.md`.
#![doc = include_str!("../README.md")]
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
pub mod rollup;
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
