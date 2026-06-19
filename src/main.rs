//! `drm3-rpc-pool` - config-driven, language-agnostic JSON-RPC failover proxy.
//!
//! Point any app, in any language, at this daemon's `listen` address as its RPC
//! URL. Every incoming JSON-RPC request is dispatched through the pool with
//! first-success-wins failover, 429/5xx detection, capability routing,
//! per-endpoint auth, and rate limiting.
//!
//!   drm3-rpc-pool --config rpc-pool.toml      # run the proxy
//!   drm3-rpc-pool init base > rpc-pool.toml   # write a starter config
//!
//! Endpoints `/health` and `/metrics` are served alongside the JSON-RPC root.

use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use drm3_rpc_pool::{presets, proxy, RpcPool, RpcPoolConfig};

#[derive(Parser, Debug)]
#[command(
    name = "drm3-rpc-pool",
    version,
    about = "Resilient JSON-RPC failover proxy for any EVM chain"
)]
struct Cli {
    /// Path to the TOML config file. Defaults to ./rpc-pool.toml when running
    /// the proxy with no subcommand.
    #[arg(short, long, global = true)]
    config: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Write a starter config to stdout from a chain preset.
    Init {
        /// Chain preset: base, ethereum, arbitrum, optimism, polygon, bnb.
        chain: String,
    },
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Init { chain }) => init(&chain),
        None => serve(cli.config.as_deref().unwrap_or("rpc-pool.toml")).await,
    }
}

/// Emit a starter config for `chain` to stdout.
fn init(chain: &str) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = presets::config_for(chain).ok_or_else(|| {
        format!(
            "unknown chain {chain:?}. known presets: {}",
            presets::names().join(", ")
        )
    })?;
    let body = cfg.to_toml_string()?;
    let header = format!(
        "# drm3-rpc-pool starter config for `{chain}`.\n\
         # Add your own keyed endpoints at a lower `priority` so paid capacity\n\
         # is preferred and the public URLs below act as failover. Secrets go\n\
         # in the environment via ${{ENV_VAR}} templating, e.g.:\n\
         #\n\
         #   [[endpoints]]\n\
         #   url = \"https://eth-mainnet.g.alchemy.com/v2/${{ALCHEMY_KEY}}\"\n\
         #   label = \"alchemy\"\n\
         #   priority = 0\n\
         #   auth = {{ type = \"url_key\" }}\n\
         #\n"
    );
    print!("{header}\n{body}");
    Ok(())
}

/// Load config and run the HTTP proxy server.
async fn serve(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let cfg = RpcPoolConfig::from_toml_file(config_path)?;
    let listen = cfg.listen.clone();
    let endpoint_count = cfg.endpoints.len();
    let pool = RpcPool::from_config(cfg)?;

    let app = proxy::build_router(pool);

    let addr: SocketAddr = listen
        .parse()
        .map_err(|e| format!("invalid listen address {listen:?}: {e}"))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        %addr,
        endpoints = endpoint_count,
        "drm3-rpc-pool proxy listening"
    );
    eprintln!("drm3-rpc-pool listening on http://{addr} ({endpoint_count} endpoints)");

    axum::serve(listener, app).await?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("drm3_rpc_pool=info,info"));
    let _ = fmt().with_env_filter(filter).try_init();
}
