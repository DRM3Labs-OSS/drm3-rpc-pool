# drm3-rpc-pool

[![CI](https://github.com/DRM3Labs-OSS/drm3-rpc-pool/actions/workflows/ci.yml/badge.svg)](https://github.com/DRM3Labs-OSS/drm3-rpc-pool/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)

> Config-driven JSON-RPC failover proxy for any EVM chain. Point any app at one endpoint and get automatic failover, health-aware routing, and rate-limit handling across your RPC providers. Rust library + WASM/TS package.

## The problem

A single hardcoded RPC URL is a single point of failure. Public providers rate-limit you (HTTP 429), lag under load, and go down without warning, and when yours does, your app goes down with it. Hardcoding one endpoint means betting your uptime on someone else's worst day.

drm3-rpc-pool fixes this without touching your app code: give it an ordered list of endpoints and point your app at the proxy. It does first-success-wins dispatch with per-endpoint health tracking, exponential backoff, capability routing, and automatic failover. Every capability is a generic `eth_*` method, so it works the same on Ethereum, Base, Arbitrum, Optimism, Polygon, BNB, or any other EVM chain. You do **not** need to write Rust to use it. Run the proxy, point any app in any language at its local address, and it inherits failover for free.

## Results: with vs without

The chart and table below come from a **real-network pool-size sweep**, rendered by the benchmark tool and refreshed by [`benchmark.yml`](./.github/workflows/benchmark.yml) (run it from the Actions tab, or it runs weekly). We fire the same concurrent burst at a pool of 1 provider (single endpoint, no failover), then 2, then 3, on up to all preset endpoints. One public endpoint gets rate-limited (HTTP 429) under the burst and collapses; the moment a second provider is in the pool, failover routes around the throttled endpoint, the burst is absorbed, and sustained throughput jumps. At this burst size a pool of 2 already soaks up the load, so the success-rate gain past 2 providers is small and noisy; the reliable signal is throughput climbing as load spreads. Numbers are timestamped and vary with live public-RPC conditions; this is honest field data, not a controlled lab benchmark. Public endpoints are IP-rate-limited, so where it matters each pool size runs on its own CI runner (its own IP), and heavier bursts push the success-rate ceiling out to more providers. Adding your own keyed provider raises the ceiling well past what these no-key public endpoints can do.

<!-- BENCHMARK:START -->
**1000 requests · concurrency 250 · chain `base` · FREE public endpoints (no key)**

_Method: we fire **1000 requests** at **concurrency 250** (250 calls in flight at once) against a pool of 1 provider, then 2, then 3, on up. `Throughput (req/s)` is **successful** `eth_blockNumber` calls per second sustained over the burst (ok-only: failed/rate-limited calls are excluded), not a count of requests. This is single-runner, real-network field data, not a controlled lab benchmark._

![One provider buckles, a pool holds: success rate and throughput across pool sizes](./assets/benchmark.svg)

| Providers | Mode | Success rate | Throughput (req/s) | p50 latency | p95 latency |
|----------:|------|-------------:|-------------------:|------------:|------------:|
| 1 | single (no failover) | 15.9% | 224 | 225 ms | 487 ms |
| 2 | pool (failover) | 100.0% | 1384.7 | 74 ms | 493 ms |
| 3 | pool (failover) | 86.7% | 203.1 | 105 ms | 475 ms |
| 4 | pool (failover) | 97.6% | 274.9 | 85 ms | 521 ms |
| 5 | pool (failover) | 99.8% | 690.5 | 76 ms | 492 ms |

_Auto-generated 2026-06-18 00:16:07 UTC. Real-network field data against free public endpoints, not a lab benchmark; numbers vary with live public-RPC conditions. A single public endpoint gets rate-limited (HTTP 429) under this burst and collapses; with a pool, failover routes around the throttled endpoint so the burst is absorbed and sustained throughput climbs as load spreads across providers. At this load a pool of 2 already absorbs the burst, so the success-rate gain past 2 is small and noisy (public endpoints share one IP on a single runner, and their rate-limit windows overlap run to run); the climb to watch is throughput. Heavier bursts push the success-rate ceiling out to more providers._

_Run it yourself: `cargo run --release --example throughput` (see [Throughput benchmark](#throughput-benchmark))._
<!-- BENCHMARK:END -->

## How it works

```mermaid
sequenceDiagram
    participant App as Your app (any language)
    participant Pool as drm3-rpc-pool proxy
    participant A as Endpoint A (priority 0)
    participant B as Endpoint B (priority 1)

    App->>Pool: POST eth_call
    Pool->>A: forward eth_call
    A-->>Pool: HTTP 429 (non-2xx counts as a failure for A)
    Pool->>B: fail over to next healthy, capable endpoint
    B-->>Pool: HTTP 200 result
    Pool-->>App: JSON-RPC result (first success wins)
    Pool->>A: after 2 failures, demote A into exponential backoff
    A-->>Pool: skipped while cooling down, one success restores it
```

- **Unbounded, priority-ordered pool.** Lower `priority` is tried first; ties break by config order.
- **First-success-wins dispatch.** Try candidates in order, return the moment one succeeds.
- **Failure detection.** Any non-2xx response (429 rate-limit, 5xx, etc.) or transport error counts as a failure for that endpoint.
- **Demotion + backoff.** After `demotion_threshold` (default 2) consecutive failures an endpoint is demoted into an exponentially growing cooldown (base 2s, doubling per failure, capped at 300s). One success resets it.
- **Capability routing.** Tag an endpoint with the methods it supports and the pool skips it for calls it cannot serve. An empty list means "supports everything".
- **Per-endpoint rate limiting.** Optional client-side `max_rps`; a locally-throttled endpoint is skipped (failed over), never awaited.
- **Per-endpoint auth.** URL-baked keys, custom headers, or bearer tokens, with secrets pulled from the environment.

## Install

Nothing is published to a package registry yet, so the two paths that work **today** are building from source and the git dependency. The registry and release options below are wired up and will work once the first version is tagged and published.

### From source (works today)

Clone and build the proxy with Cargo (needs a stable Rust toolchain):

```sh
git clone https://github.com/DRM3Labs-OSS/drm3-rpc-pool
cd drm3-rpc-pool
cargo build --release
# binary lands at ./target/release/drm3-rpc-pool
./target/release/drm3-rpc-pool init base > rpc-pool.toml
./target/release/drm3-rpc-pool --config rpc-pool.toml
```

Or install the proxy onto your `PATH`:

```sh
cargo install --path .
drm3-rpc-pool init base > rpc-pool.toml   # write a starter config
drm3-rpc-pool --config rpc-pool.toml      # run the proxy (defaults to ./rpc-pool.toml)
```

The binary has one subcommand, `init <chain>` (writes a starter config to stdout), and otherwise runs the proxy; see [Primary usage](#primary-usage-the-proxy-no-rust-required).

### As a Rust library

After the first crates.io release:

```sh
cargo add drm3-rpc-pool
```

The git dependency works **today**, before any crates.io release - add to your `Cargo.toml`:

```toml
[dependencies]
drm3-rpc-pool = { git = "https://github.com/DRM3Labs-OSS/drm3-rpc-pool" }
```

For a pure library build with no proxy daemon, disable default features and bring your own transport: `{ git = "...", default-features = false }`. See [Secondary usage: the Rust library](#secondary-usage-the-rust-library).

### As a JS/TS package (browser + Node)

After the first npm release, the WASM binding ([`bindings/wasm`](./bindings/wasm)) publishes as a scoped package:

```sh
npm i @drm3labs-oss/rpc-pool
```

It runs failover, per-endpoint health, backoff, and capability routing in the browser or Node; the network calls are done by the platform `fetch`. Until that release lands, build it locally from `bindings/wasm` with `wasm-pack` (`npm run build:web` / `build:node`).

### Prebuilt binary

After the first tagged release (`v*`), prebuilt proxy binaries for linux (x64 + arm64), macOS (x64 + arm64), and Windows (x64) are attached to each [GitHub Release](https://github.com/DRM3Labs-OSS/drm3-rpc-pool/releases) alongside `SHA256SUMS`. Download, verify, and run:

```sh
# (available once a vX.Y.Z tag has been pushed)
tar xzf drm3-rpc-pool-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz
./drm3-rpc-pool --config rpc-pool.toml
```

## Primary usage: the proxy (no Rust required)

### 1. Scaffold a config

```sh
drm3-rpc-pool init base > rpc-pool.toml
```

`init` writes a starter config from a chain preset. Available presets: `base`, `ethereum`, `arbitrum`, `optimism`, `polygon`, `bnb` (aliases: `eth`/`mainnet`, `arb`, `op`, `matic`, `bsc`).

### 2. Edit endpoints and keys

Open `rpc-pool.toml`, add your own keyed providers at a lower `priority` so paid capacity is preferred and the public URLs act as failover. Put secrets in the environment via `${ENV_VAR}` templating (see [Auth](#auth--keyed-providers)). A full annotated example lives in [`examples/rpc-pool.toml`](./examples/rpc-pool.toml).

### 3. Run the proxy

```sh
export ALCHEMY_KEY=...          # whatever your config references
drm3-rpc-pool --config rpc-pool.toml
# drm3-rpc-pool listening on http://127.0.0.1:8545 (N endpoints)
```

`--config` defaults to `./rpc-pool.toml`, so plain `drm3-rpc-pool` works once the file exists.

### 4. Point any app at it

Set your application's RPC URL to the proxy's `listen` address. Examples:

```sh
# ethers.js / viem / web3.py / cast / hardhat - just change the URL:
export RPC_URL=http://127.0.0.1:8545
cast block-number --rpc-url http://127.0.0.1:8545
```

Everything that speaks JSON-RPC over HTTP works unchanged - single requests and batch arrays are both supported.

### Run with Docker (optional)

```sh
docker build -t drm3-rpc-pool .

docker run --rm -p 8545:8545 \
  -e ALCHEMY_KEY=... \
  -v "$PWD/rpc-pool.toml:/etc/drm3/rpc-pool.toml:ro" \
  drm3-rpc-pool
```

The image's default command is `--config /etc/drm3/rpc-pool.toml`. To reach the proxy from outside the container, set `listen = "0.0.0.0:8545"` in your config.

> **Warning: binding `0.0.0.0` exposes an unauthenticated relay.** The proxy has no auth of its own, so anything that can reach the listen address can spend your keyed/paid upstream providers. Only bind `0.0.0.0` behind a firewall, a private network, or your own auth layer. The default `127.0.0.1` is safe.

## Config schema

Configuration is TOML. Every string field supports `${ENV_VAR}` templating; a referenced-but-unset variable is a hard error, so a missing key never silently degrades to an unauthenticated request.

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `listen` | string | `127.0.0.1:8545` | `host:port` the proxy binds to. Use `0.0.0.0:8545` only behind a firewall/auth (see the Docker warning). Ignored by library callers. |
| `request_timeout_ms` | integer | transport default (15s) | Per-request timeout applied to each upstream attempt. |
| `max_retries` | integer | `0` | Max endpoints to try per call. `0` = try every healthy, capable candidate. |
| `[[endpoints]]` | array | - | One or more endpoint tables (below). At least one is required. |

Each `[[endpoints]]` table:

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `url` | string | required | HTTP(S) JSON-RPC URL. |
| `label` | string | - | Human-readable tag for logs and `/metrics`. Falls back to the URL. |
| `priority` | integer | `0` | Sort key. Lower is tried first; ties break by config order. |
| `capabilities` | array of strings | `[]` | Methods this endpoint may serve, e.g. `["eth_call", "eth_getLogs"]`. Empty = supports everything. |
| `max_rps` | integer | unthrottled | Client-side requests-per-second throttle. |
| `auth` | table | `{ type = "none" }` | See [Auth](#auth--keyed-providers). |

```toml
listen = "127.0.0.1:8545"
request_timeout_ms = 8000
max_retries = 0

[[endpoints]]
url = "https://base-mainnet.g.alchemy.com/v2/${ALCHEMY_KEY}"
label = "alchemy-base"
priority = 0
max_rps = 25
auth = { type = "url_key" }

[[endpoints]]
url = "https://mainnet.base.org"
label = "base-official"
priority = 10
```

## Auth / keyed providers

Secrets stay in the environment via `${ENV_VAR}`; the config file only references them. The `auth` table picks how the key is applied:

```toml
# UrlKey - the API key is already baked into the URL (Alchemy / Infura style).
# Declarative marker only; no extra headers are added.
[[endpoints]]
url = "https://eth-mainnet.g.alchemy.com/v2/${ALCHEMY_KEY}"
auth = { type = "url_key" }

# Header - a custom request header.
[[endpoints]]
url = "https://rpc.example-provider.com"
auth = { type = "header", name = "X-API-Key", value = "${PROVIDER_KEY}" }

# Bearer - adds `Authorization: Bearer <token>`.
[[endpoints]]
url = "https://rpc.another-provider.com"
auth = { type = "bearer", token = "${PROVIDER_TOKEN}" }
```

Omitting `auth` is equivalent to `{ type = "none" }` - the URL is used as-is.

## Chain presets

`init <chain>` ships sensible public endpoint lists for:

| Preset | Aliases | Chain |
|--------|---------|-------|
| `base` | - | Base mainnet (8453) |
| `ethereum` | `eth`, `mainnet` | Ethereum mainnet (1) |
| `arbitrum` | `arb` | Arbitrum One (42161) |
| `optimism` | `op` | OP Mainnet (10) |
| `polygon` | `matic` | Polygon PoS (137) |
| `bnb` | `bsc` | BNB Smart Chain (56) |

Presets are a starting point of public, no-key endpoints. Public endpoints come and go - treat them as defaults to override with your own keyed providers.

## HTTP endpoints

The proxy serves three routes:

- `POST /` - the JSON-RPC entrypoint. Accepts a single request object or a batch array. The client's `id` is preserved; upstream JSON-RPC error envelopes and pool-level errors (e.g. `-32010` all upstreams failed, `-32011` no healthy endpoints) are relayed back.
- `GET /health` - `200` with `{ status, endpoints_total, endpoints_healthy }` when the pool has endpoints; `503` if empty.
- `GET /metrics` - per-endpoint status snapshot (definition + live health) as JSON.

## Secondary usage: the Rust library

Add the crate and drive the pool directly. The default build bundles a `reqwest` transport.

```rust
use drm3_rpc_pool::{RpcEndpoint, RpcPool, RpcPoolConfig, RpcCapability};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = RpcPoolConfig {
        endpoints: vec![
            RpcEndpoint {
                url: "https://base.llamarpc.com".into(),
                label: Some("llamarpc".into()),
                priority: 0,
                capabilities: vec![RpcCapability::EthCall, RpcCapability::EthBlockNumber],
                max_rps: None,
                auth: Default::default(),
            },
            RpcEndpoint {
                url: "https://mainnet.base.org".into(),
                label: Some("base-public".into()),
                priority: 1,
                capabilities: vec![RpcCapability::EthCall, RpcCapability::EthGetLogs],
                max_rps: Some(10),
                auth: Default::default(),
            },
        ],
        ..RpcPoolConfig::default()
    };

    let pool = RpcPool::with_default_transport(config);
    let block: serde_json::Value = pool.call("eth_blockNumber", json!([])).await?;
    println!("block = {}", block);
    Ok(())
}
```

You can also build a config from URLs (`RpcPoolConfig::from_urls([...])`), from a preset (`presets::config_for("base")`), or from a TOML file (`RpcPoolConfig::from_toml_file(path)`). Supply your own HTTP client by implementing the `Transport` trait, and your own observability via the `Metrics` trait.

### Cargo features

- `reqwest-transport` (default) - bundled `reqwest` HTTP transport. Disable to supply your own `Transport`.
- `daemon` (default) - the proxy binary and HTTP server (axum + clap). Disable for a pure library build.

## Throughput benchmark

The [`throughput` example](./examples/throughput.rs) reproduces the [Results](#results-with-vs-without) numbers. It fires N concurrent `eth_blockNumber` requests at a pool and reports success-rate, throughput (req/s), and p50/p95 latency as JSON. The default mode is a **pool-size sweep**:

```sh
# Sweep: pool size 1, 2, 3, ... up to all preset endpoints. One JSON line per size.
cargo run --release --example throughput -- --mode sweep --chain base --requests 600 --concurrency 120
```

Each pool size prints a single JSON line carrying its `pool_size`, e.g.:

```json
{"mode":"single","chain":"base","pool_size":1,"requests":600,"concurrency":120,"ok":150,"err":450,"success_rate":0.25,"elapsed_s":0.33,"throughput_rps":456.3,"p50_ms":190,"p95_ms":220}
{"mode":"pool","chain":"base","pool_size":2,"requests":600,"concurrency":120,"ok":600,"err":0,"success_rate":1.0,"elapsed_s":1.3,"throughput_rps":463.0,"p50_ms":185,"p95_ms":657}
```

You can also run a single point:

```sh
# One endpoint only (no pool, no failover):
cargo run --release --example throughput -- --mode single --chain base --requests 600 --concurrency 120

# Exactly the first N preset endpoints (lets each pool size run on its own runner):
cargo run --release --example throughput -- --mode pool --chain base --pool-size 3 --requests 600 --concurrency 120
```

Flags: `--mode single|pool|sweep`, `--chain <preset>`, `--requests <N>`, `--concurrency <N>`, `--max-pool <N>` (sweep cap), `--pool-size <N>` (pool mode: use exactly N endpoints), `--endpoint <url>` (override the single-mode target). It uses **free public endpoints** - no key required. Public RPCs are IP-rate-limited, so a local back-to-back sweep inserts an inter-size cooldown (`SWEEP_COOLDOWN_S`, default 8s) and CI runs each pool size on its own runner (its own IP); that's why [`benchmark.yml`](./.github/workflows/benchmark.yml) splits the sizes into a matrix of separate jobs. Supplying your own keyed provider in a config raises the ceiling well past these public defaults.

## License

MIT © DRM3 Labs Corp.

Contributions welcome - see [CONTRIBUTING.md](./CONTRIBUTING.md).
