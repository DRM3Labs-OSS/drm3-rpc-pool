//! Throughput benchmark: pool-size sweep (1 provider -> N providers).
//!
//! Fires N concurrent `eth_blockNumber` requests at a pool and reports
//! success-rate, throughput (req/s), and p50/p95 latency as a single JSON line.
//! Modes:
//!
//!   * `--mode single` — all load at ONE public endpoint (pool size 1, no
//!     failover). The control case.
//!   * `--mode pool`   — same load spread across the chain preset's full pool,
//!     with per-endpoint health, backoff, and automatic failover.
//!   * `--mode sweep`  — run pool size 1, 2, 3, … up to `--max-pool` (default:
//!     all preset endpoints) and print ONE JSON line per pool size, each with a
//!     `pool_size` field. This is the strong proof: success rate climbs as
//!     providers are added.
//!
//! Public RPCs are IP-rate-limited, so distinct modes/pool-sizes are meant to
//! run on separate machines / CI runners (separate IPs) to avoid sharing limits
//! where the comparison must be apples-to-apples. See
//! `.github/workflows/benchmark.yml`.
//!
//! Usage:
//!   cargo run --release --example throughput -- \
//!       --mode sweep --chain base --requests 600 --concurrency 120
//!
//! Flags:
//!   --mode single|pool|sweep   (default: sweep)
//!   --chain <preset>           (default: base; any preset name/alias)
//!   --requests <N>             (default: 600)
//!   --concurrency <N>          (default: 120)
//!   --max-pool <N>             (sweep only: cap pool size; default: all)
//!   --pool-size <N>            (run a single pool size of exactly N endpoints)
//!   --endpoint <url>           (single mode only: override the target endpoint)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use drm3_rpc_pool::{presets, RpcEndpoint, RpcPool, RpcPoolConfig};
use serde_json::{json, Value};

struct Args {
    mode: String,
    chain: String,
    requests: usize,
    concurrency: usize,
    max_pool: Option<usize>,
    pool_size: Option<usize>,
    endpoint: Option<String>,
}

fn parse_args() -> Args {
    let mut mode = "sweep".to_string();
    let mut chain = "base".to_string();
    let mut requests = 600usize;
    let mut concurrency = 120usize;
    let mut max_pool: Option<usize> = None;
    let mut pool_size: Option<usize> = None;
    let mut endpoint: Option<String> = None;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--mode" => mode = it.next().unwrap_or(mode),
            "--chain" => chain = it.next().unwrap_or(chain),
            "--requests" => requests = it.next().and_then(|v| v.parse().ok()).unwrap_or(requests),
            "--concurrency" => {
                concurrency = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(concurrency)
            }
            "--max-pool" => max_pool = it.next().and_then(|v| v.parse().ok()),
            "--pool-size" => pool_size = it.next().and_then(|v| v.parse().ok()),
            "--endpoint" => endpoint = it.next(),
            "-h" | "--help" => {
                eprintln!(
                    "throughput --mode single|pool|sweep --chain <preset> \
                     --requests N --concurrency N [--max-pool N] [--pool-size N] [--endpoint URL]"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown flag: {other}");
                std::process::exit(2);
            }
        }
    }

    Args {
        mode,
        chain,
        requests,
        concurrency,
        max_pool,
        pool_size,
        endpoint,
    }
}

fn percentile(sorted_ms: &[u64], p: f64) -> u64 {
    if sorted_ms.is_empty() {
        return 0;
    }
    let rank = (p * (sorted_ms.len() as f64 - 1.0)).round() as usize;
    sorted_ms[rank.min(sorted_ms.len() - 1)]
}

fn preset_endpoints(chain: &str) -> Vec<RpcEndpoint> {
    presets::endpoints_for(chain).unwrap_or_else(|| {
        eprintln!("unknown chain preset: {chain}");
        std::process::exit(2);
    })
}

/// Fire `requests` calls at `pool`, bounded to `concurrency` in flight, and
/// return the measured report fields as a JSON object (without identity fields).
async fn measure(pool: RpcPool, requests: usize, concurrency: usize) -> Value {
    let ok = Arc::new(AtomicU64::new(0));
    let err = Arc::new(AtomicU64::new(0));
    let latencies: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::with_capacity(requests)));

    let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let start = Instant::now();

    let mut handles = Vec::with_capacity(requests);
    for _ in 0..requests {
        let pool = pool.clone();
        let ok = ok.clone();
        let err = err.clone();
        let latencies = latencies.clone();
        let sem = sem.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore not closed");
            let t0 = Instant::now();
            match pool.call("eth_blockNumber", json!([])).await {
                Ok(_) => {
                    ok.fetch_add(1, Ordering::Relaxed);
                    latencies
                        .lock()
                        .expect("latency mutex poisoned")
                        .push(t0.elapsed().as_millis() as u64);
                }
                Err(_) => {
                    err.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let elapsed = start.elapsed();
    let ok = ok.load(Ordering::Relaxed);
    let err = err.load(Ordering::Relaxed);
    let total = ok + err;

    let mut lat = latencies.lock().expect("latency mutex poisoned").clone();
    lat.sort_unstable();

    let elapsed_s = elapsed.as_secs_f64();
    let success_rate = if total == 0 {
        0.0
    } else {
        ok as f64 / total as f64
    };
    let throughput_rps = if elapsed_s > 0.0 {
        ok as f64 / elapsed_s
    } else {
        0.0
    };

    json!({
        "ok": ok,
        "err": err,
        "success_rate": (success_rate * 1000.0).round() / 1000.0,
        "elapsed_s": (elapsed_s * 100.0).round() / 100.0,
        "throughput_rps": (throughput_rps * 10.0).round() / 10.0,
        "p50_ms": percentile(&lat, 0.50),
        "p95_ms": percentile(&lat, 0.95),
    })
}

/// Merge identity fields onto a measured report and print it as one JSON line.
fn emit(mode: &str, chain: &str, requests: usize, concurrency: usize, pool_size: usize, m: Value) {
    let mut out = json!({
        "mode": mode,
        "chain": chain,
        "pool_size": pool_size,
        "requests": requests,
        "concurrency": concurrency,
    });
    if let (Value::Object(dst), Value::Object(src)) = (&mut out, m) {
        dst.extend(src);
    }
    println!("{out}");
}

#[tokio::main]
async fn main() {
    let args = parse_args();

    match args.mode.as_str() {
        // Whole-preset pool, full failover. With `--pool-size N`, take exactly
        // the first N preset endpoints (lets CI run one pool size per runner).
        // pool_size = number of endpoints actually wired up.
        "pool" => {
            let mut eps = preset_endpoints(&args.chain);
            if let Some(n) = args.pool_size {
                eps.truncate(n.clamp(1, eps.len()));
            }
            let size = eps.len();
            let pool = RpcPool::with_default_transport(RpcPoolConfig {
                endpoints: eps,
                ..RpcPoolConfig::default()
            });
            let m = measure(pool, args.requests, args.concurrency).await;
            emit(
                "pool",
                &args.chain,
                args.requests,
                args.concurrency,
                size,
                m,
            );
        }

        // One endpoint only (pool size 1): the override, else the preset's first.
        "single" => {
            let url = match &args.endpoint {
                Some(u) => u.clone(),
                None => {
                    preset_endpoints(&args.chain)
                        .into_iter()
                        .next()
                        .expect("preset has at least one endpoint")
                        .url
                }
            };
            let pool = RpcPool::with_default_transport(RpcPoolConfig::from_urls([url]));
            let m = measure(pool, args.requests, args.concurrency).await;
            emit("single", &args.chain, args.requests, args.concurrency, 1, m);
        }

        // Pool-size sweep: 1, 2, … up to max-pool (default: all preset endpoints).
        // One JSON line per size, each with its own `pool_size`.
        "sweep" => {
            let eps = preset_endpoints(&args.chain);
            let cap = args.max_pool.unwrap_or(eps.len()).clamp(1, eps.len());
            // Cooldown between sizes so a prior burst's IP rate-limiting does
            // not bleed into the next size on a shared-IP run (e.g. local). On
            // separate CI runners each size has its own IP and this is moot.
            let cooldown_s: u64 = std::env::var("SWEEP_COOLDOWN_S")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8);
            for size in 1..=cap {
                if size > 1 && cooldown_s > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(cooldown_s)).await;
                }
                let subset: Vec<RpcEndpoint> = eps.iter().take(size).cloned().collect();
                let mode = if size == 1 { "single" } else { "pool" };
                let pool = RpcPool::with_default_transport(RpcPoolConfig {
                    endpoints: subset,
                    ..RpcPoolConfig::default()
                });
                let m = measure(pool, args.requests, args.concurrency).await;
                emit(mode, &args.chain, args.requests, args.concurrency, size, m);
            }
        }

        other => {
            eprintln!("unknown mode: {other} (expected single|pool|sweep)");
            std::process::exit(2);
        }
    }
}
