//! Throughput benchmark: with vs without the pool.
//!
//! Fires N concurrent `eth_blockNumber` requests two ways and reports
//! success-rate, throughput (req/s), and p50/p95 latency as a single JSON line:
//!
//!   * `--mode single` — all load at ONE public endpoint (no pool, no failover).
//!   * `--mode pool` — same load spread across the chain preset's pool, with
//!     per-endpoint health, backoff, and automatic failover.
//!
//! Public RPCs are IP-rate-limited, so the two modes are meant to run on
//! separate machines / CI runners (separate IPs) to avoid sharing limits. See
//! `.github/workflows/benchmark.yml`.
//!
//! Usage:
//!   cargo run --release --example throughput -- \
//!       --mode pool --chain base --requests 300 --concurrency 50
//!
//! Flags:
//!   --mode single|pool   (default: pool)
//!   --chain <preset>     (default: base; any preset name/alias)
//!   --requests <N>       (default: 300)
//!   --concurrency <N>    (default: 50)
//!   --endpoint <url>     (single mode only: override the target endpoint)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use drm3_rpc_pool::{presets, RpcPool, RpcPoolConfig};
use serde_json::json;

struct Args {
    mode: String,
    chain: String,
    requests: usize,
    concurrency: usize,
    endpoint: Option<String>,
}

fn parse_args() -> Args {
    let mut mode = "pool".to_string();
    let mut chain = "base".to_string();
    let mut requests = 300usize;
    let mut concurrency = 50usize;
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
            "--endpoint" => endpoint = it.next(),
            "-h" | "--help" => {
                eprintln!(
                    "throughput --mode single|pool --chain <preset> \
                     --requests N --concurrency N [--endpoint URL]"
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

#[tokio::main]
async fn main() {
    let args = parse_args();

    // Build the pool for the requested mode.
    let pool = match args.mode.as_str() {
        "pool" => {
            let config = presets::config_for(&args.chain).unwrap_or_else(|| {
                eprintln!("unknown chain preset: {}", args.chain);
                std::process::exit(2);
            });
            RpcPool::with_default_transport(config)
        }
        "single" => {
            // One endpoint only: the override, else the preset's first endpoint.
            let url = match &args.endpoint {
                Some(u) => u.clone(),
                None => {
                    let eps = presets::endpoints_for(&args.chain).unwrap_or_else(|| {
                        eprintln!("unknown chain preset: {}", args.chain);
                        std::process::exit(2);
                    });
                    eps.into_iter()
                        .next()
                        .expect("preset has at least one endpoint")
                        .url
                }
            };
            RpcPool::with_default_transport(RpcPoolConfig::from_urls([url]))
        }
        other => {
            eprintln!("unknown mode: {other} (expected single|pool)");
            std::process::exit(2);
        }
    };

    let ok = Arc::new(AtomicU64::new(0));
    let err = Arc::new(AtomicU64::new(0));
    let latencies: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::with_capacity(args.requests)));

    let sem = Arc::new(tokio::sync::Semaphore::new(args.concurrency));
    let start = Instant::now();

    let mut handles = Vec::with_capacity(args.requests);
    for _ in 0..args.requests {
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

    let report = json!({
        "mode": args.mode,
        "chain": args.chain,
        "requests": args.requests,
        "concurrency": args.concurrency,
        "ok": ok,
        "err": err,
        "success_rate": (success_rate * 1000.0).round() / 1000.0,
        "elapsed_s": (elapsed_s * 100.0).round() / 100.0,
        "throughput_rps": (throughput_rps * 10.0).round() / 10.0,
        "p50_ms": percentile(&lat, 0.50),
        "p95_ms": percentile(&lat, 0.95),
    });

    println!("{report}");
}
