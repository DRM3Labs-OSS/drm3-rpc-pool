# AGENTS.md - drm3-rpc-pool

Operational, as-built guide. Enough to run the proxy or use the library
immediately.

## What this is

A JSON-RPC endpoint pool for any EVM chain: failover, per-endpoint health, and
load spreading across providers. Primarily a **library**; a proxy is included
for apps that aren't Rust or TS.

1. **Rust library (`drm3_rpc_pool`)** - embed `RpcPool` directly. The main path.
2. **TypeScript/WASM (`@drm3labs-oss/rpc-pool`)** - the same pool in the browser
   and Node, over `fetch`. See `bindings/wasm`.
3. **Proxy daemon (`drm3-rpc-pool`)** - an HTTP server for any other language;
   point an app at its `listen` address as the RPC URL. The fallback, not the
   main path.

Routing: candidates are ordered `(saturated, priority, in-flight, index)` -
lower priority preferred, equal priority = peers (load spreads by least
in-flight), `max_in_flight` cap spills a saturated endpoint to peers. No
chain-specific code: every capability is a generic `eth_*` method.

## Build

```sh
cargo build --all-features          # proxy binary + library
cargo build --release --bin drm3-rpc-pool
```

The `daemon` feature (default-on) builds the binary; `reqwest-transport`
(default-on) provides the bundled HTTP client. For a pure library build with a
custom transport: `cargo build --no-default-features --features reqwest-transport`
or drop `reqwest-transport` too and implement `Transport` yourself.

## Run - the proxy

```sh
drm3-rpc-pool init base > rpc-pool.toml   # scaffold from a preset
export ALCHEMY_KEY=...                     # set whatever the config references
drm3-rpc-pool --config rpc-pool.toml       # run (defaults to ./rpc-pool.toml)
```

CLI surface (clap):

- `drm3-rpc-pool [--config <path>]` - run the proxy. No subcommand = serve.
  `--config`/`-c` defaults to `./rpc-pool.toml`.
- `drm3-rpc-pool init <chain>` - print a starter config to stdout. Chains:
  `base`, `ethereum`, `arbitrum`, `optimism`, `polygon`, `bnb` (aliases
  `eth`/`mainnet`, `arb`, `op`, `matic`, `bsc`).
- `--version` from the crate version.

Logging: `tracing` via `RUST_LOG`/`EnvFilter`, default `drm3_rpc_pool=info,info`.

### Run - Docker

```sh
docker build -t drm3-rpc-pool .
docker run --rm -p 8545:8545 \
  -e ALCHEMY_KEY=... \
  -v "$PWD/rpc-pool.toml:/etc/drm3/rpc-pool.toml:ro" \
  drm3-rpc-pool
```

Default command is `--config /etc/drm3/rpc-pool.toml`. Use
`listen = "0.0.0.0:8545"` in the config so it is reachable from outside the
container. Multi-stage build, runs as non-root uid 10001, rustls (CA certs only).

## HTTP endpoints (proxy)

- `POST /` - JSON-RPC. Single object or batch array. Preserves the client `id`.
  Relays upstream error envelopes; pool errors map to `-32010` (all upstreams
  failed), `-32011` (no healthy endpoints), `-32603` (other), `-32600` (missing
  method).
- `GET /health` - `200 {status, endpoints_total, endpoints_healthy}` when the
  pool has endpoints, else `503`.
- `GET /metrics` - `{ endpoints: [...] }` per-endpoint definition + live health.

## Config (TOML)

Top-level: `listen` (default `127.0.0.1:8545`), `request_timeout_ms` (default
transport 15s), `max_retries` (default `0` = try every healthy/capable
candidate), and one or more `[[endpoints]]`.

Per endpoint: `url` (required), `label`, `priority` (default 0, lower tried
first), `capabilities` (default `[]` = supports everything), `max_rps` (client
throttle), `auth`.

Auth modes (`auth = { type = ... }`): `none` (default), `url_key` (key baked in
URL, no extra headers), `header` (`{ name, value }`), `bearer` (`{ token }`).

Every string field supports `${ENV_VAR}` templating; an unset referenced var is
a hard error. See [`examples/rpc-pool.toml`](./examples/rpc-pool.toml).

## Use - the library

```rust
use drm3_rpc_pool::{RpcPool, RpcPoolConfig};
use serde_json::json;

let cfg = RpcPoolConfig::from_urls(["https://base.llamarpc.com", "https://mainnet.base.org"]);
let pool = RpcPool::with_default_transport(cfg);
let n = pool.call("eth_blockNumber", json!([])).await?;
```

Key entry points: `RpcPool::with_default_transport`, `RpcPool::from_config`
(honors `request_timeout_ms`), `RpcPool::new` (custom transport + metrics +
backoff). Dispatch: `pool.call(method, params) -> Value`,
`pool.forward(method, params) -> ForwardResult` (used by the proxy). Inspect:
`pool.status()`, `pool.endpoints()`, `pool.len()`. Config helpers:
`RpcPoolConfig::from_urls`, `from_toml_str`, `from_toml_file`, `to_toml_string`;
`presets::config_for(name)`. Extension traits: `Transport`, `Metrics`.

## Module layout (`src/`)

- `main.rs` - CLI + proxy bootstrap (clap, tokio, tracing).
- `lib.rs` - public re-exports; feature gating.
- `config.rs` - `RpcPoolConfig`, `RpcEndpoint`, `Auth`, `RpcCapability`, env templating.
- `presets.rs` - per-chain public endpoint lists + name/alias resolution.
- `proxy.rs` - axum router, `POST /`, `/health`, `/metrics` (feature `daemon`).
- `pool/` - `mod.rs` (dispatch/failover), `rate_limit.rs`, `wire.rs`, `tests.rs`.
- `health.rs` - `BackoffPolicy`, `EndpointHealth`, demotion/cooldown.
- `transport.rs` - `Transport` trait + bundled `ReqwestTransport`.
- `metrics.rs` - `Metrics` trait + `NoopMetrics`.
- `error.rs` - `RpcError`.

## Failover semantics

Candidates walked in priority order; skipped if incapable, in cooldown, or
locally rate-limited. Any non-2xx (429/5xx) or transport error =
`record_failure`. After `demotion_threshold` (default 2) consecutive failures
the endpoint is demoted into exponential backoff (base 2s, doubling, cap 300s).
One success resets the counter. First success wins.

## Conventions

- All code is Rust; TypeScript/JS not involved here.
- Keep `cargo fmt --all --check`, `clippy --all-targets --all-features -D warnings`,
  and `cargo test --all-features` green. CI runs all three.
- Chain-agnostic: no chain-specific logic; capabilities are generic `eth_*`.
- Secrets only via `${ENV_VAR}`; never literal keys in config or source.
