//! Field load test: pound a real pool and report success rate + throughput.
//!
//! Fires `--requests` calls at `--concurrency` in flight against the first
//! `--pool-size` endpoints of a chain preset (or sweeps 1..N), and prints one
//! JSON line per pool size with success rate, throughput (req/s), and p50/p95
//! latency. `--runs K` repeats each measurement K times and reports the median
//! with a min..max band; `--warmup` discards a priming burst first.
//!
//! `--route` selects how load is distributed across the pool:
//! - `spread` (default): equal-priority peers, least in-flight wins.
//! - `chain`: distinct priorities, strict first-success failover.
//! - `capped`: ranked, with a `--cap N` soft in-flight ceiling that spills to
//!   the next endpoint when full.
//!
//! These are real public endpoints, so numbers are noisy and vary with live
//! conditions: free RPCs rate-limit per IP and some are flaky. The robust,
//! repeatable signal is the SUCCESS RATE (a single endpoint drops calls under a
//! burst; a pool of two or more completes them). Throughput does not reliably
//! scale across free public endpoints - point this at endpoints with their own
//! independent capacity (your keyed providers) to see throughput aggregate.
//!
//! Usage:
//!   cargo run --release --example throughput -- \
//!       --mode sweep --max-pool 5 --route spread --runs 3 --warmup \
//!       --chain base --requests 500 --concurrency 120
//!
//! Flags:
//!   --mode single|pool|sweep   (default: sweep)
//!   --route spread|chain|capped(default: spread; capped needs --cap N)
//!   --cap <N>                  (capped route: per-endpoint soft in-flight cap)
//!   --pass pinned|shuffled     (default: pinned; shuffled permutes order per run)
//!   --runs <K>                 (median of K bursts; default: 1)
//!   --warmup                   (fire one extra burst first and discard it)
//!   --seed <N>                 (shuffled-pass PRNG seed; default: 1)
//!   --chain <preset>           (default: base; any preset name/alias)
//!   --requests <N>             (default: 500)
//!   --concurrency <N>          (default: 120)
//!   --max-pool <N>             (sweep only: cap pool size; default: all)
//!   --pool-size <N>            (pool/single mode: use exactly N endpoints)
//!   --endpoint <url>           (single mode only: override the target endpoint)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use drm3_rpc_pool::{presets, RpcEndpoint, RpcPool, RpcPoolConfig};
use serde_json::{json, Value};

struct Args {
    mode: String,
    route: String,
    cap: Option<u32>,
    pass: String,
    runs: usize,
    warmup: bool,
    seed: u64,
    chain: String,
    requests: usize,
    concurrency: usize,
    max_pool: Option<usize>,
    pool_size: Option<usize>,
    endpoint: Option<String>,
}

fn parse_args() -> Args {
    let mut mode = "sweep".to_string();
    let mut route = "spread".to_string();
    let mut cap: Option<u32> = None;
    let mut pass = "pinned".to_string();
    let mut runs = 1usize;
    let mut warmup = false;
    let mut seed = 1u64;
    let mut chain = "base".to_string();
    let mut requests = 500usize;
    let mut concurrency = 120usize;
    let mut max_pool: Option<usize> = None;
    let mut pool_size: Option<usize> = None;
    let mut endpoint: Option<String> = None;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--mode" => mode = it.next().unwrap_or(mode),
            "--route" => route = it.next().unwrap_or(route),
            "--cap" => cap = it.next().and_then(|v| v.parse().ok()),
            "--pass" => pass = it.next().unwrap_or(pass),
            "--runs" => {
                runs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(runs)
                    .max(1)
            }
            "--warmup" => warmup = true,
            "--seed" => seed = it.next().and_then(|v| v.parse().ok()).unwrap_or(seed),
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
                    "throughput --mode single|pool|sweep --route spread|chain|capped [--cap N] \
                     --pass pinned|shuffled --runs K --warmup --seed N --chain <preset> \
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
        route,
        cap,
        pass,
        runs,
        warmup,
        seed,
        chain,
        requests,
        concurrency,
        max_pool,
        pool_size,
        endpoint,
    }
}

// ── Deterministic PRNG (SplitMix64) ─────────────────────────────────────
// No external dep: a tiny, well-known generator so the shuffled pass is fully
// reproducible from `--seed`.
struct SplitMix64(u64);
impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform integer in `[0, n)` via rejection sampling (n > 0).
    fn below(&mut self, n: u64) -> u64 {
        let zone = u64::MAX - (u64::MAX % n);
        loop {
            let r = self.next_u64();
            if r < zone {
                return r % n;
            }
        }
    }
}

/// Fisher-Yates permutation of `eps` in place; priority is assigned afterwards
/// by the routing policy (see `build`).
fn permute(eps: &mut [RpcEndpoint], rng: &mut SplitMix64) {
    let len = eps.len();
    for i in (1..len).rev() {
        let j = rng.below((i + 1) as u64) as usize;
        eps.swap(i, j);
    }
}

fn percentile(sorted_ms: &[u64], p: f64) -> u64 {
    if sorted_ms.is_empty() {
        return 0;
    }
    let rank = (p * (sorted_ms.len() as f64 - 1.0)).round() as usize;
    sorted_ms[rank.min(sorted_ms.len() - 1)]
}

fn median_f64(mut xs: Vec<f64>) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    }
}

fn median_u64(mut xs: Vec<u64>) -> u64 {
    if xs.is_empty() {
        return 0;
    }
    xs.sort_unstable();
    xs[xs.len() / 2]
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}
fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn preset_endpoints(chain: &str) -> Vec<RpcEndpoint> {
    presets::endpoints_for(chain).unwrap_or_else(|| {
        eprintln!("unknown chain preset: {chain}");
        std::process::exit(2);
    })
}

/// One burst: fire `requests` calls bounded to `concurrency`; return
/// `(success_rate, throughput_rps, elapsed_s, p50_ms, p95_ms)`.
async fn one_burst(
    pool: RpcPool,
    requests: usize,
    concurrency: usize,
) -> (f64, f64, f64, u64, u64) {
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

    let elapsed_s = start.elapsed().as_secs_f64();
    let ok = ok.load(Ordering::Relaxed);
    let err = err.load(Ordering::Relaxed);
    let total = ok + err;

    let mut lat = latencies.lock().expect("latency mutex poisoned").clone();
    lat.sort_unstable();

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
    (
        success_rate,
        throughput_rps,
        elapsed_s,
        percentile(&lat, 0.50),
        percentile(&lat, 0.95),
    )
}

/// Cooldown between bursts so a prior burst's IP rate-limiting does not bleed
/// into the next on a shared IP. Override with `SWEEP_COOLDOWN_S`.
async fn cooldown() {
    let secs: u64 = std::env::var("SWEEP_COOLDOWN_S")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    if secs > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
    }
}

/// Build a pool of the first `size` preset endpoints under the route + pass
/// policy. `spread` makes equal-priority peers; `chain`/`capped` keep a ranked
/// order; `capped` also sets a soft in-flight cap. `shuffled` permutes order
/// each run (seeded) so the band reflects order sensitivity.
fn build(
    eps: &[RpcEndpoint],
    size: usize,
    route: &str,
    cap: Option<u32>,
    pass: &str,
    seed: u64,
    run: usize,
) -> RpcPool {
    let mut subset: Vec<RpcEndpoint> = eps.iter().take(size).cloned().collect();
    if pass == "shuffled" {
        let mut rng = SplitMix64(seed ^ (run as u64).wrapping_mul(0x100_0000_01B3));
        permute(&mut subset, &mut rng);
    }
    for (i, ep) in subset.iter_mut().enumerate() {
        ep.priority = if route == "spread" { 0 } else { i as u32 };
        ep.max_in_flight = if route == "capped" { cap } else { None };
    }
    RpcPool::with_default_transport(RpcPoolConfig {
        endpoints: subset,
        ..RpcPoolConfig::default()
    })
}

/// Run `runs` bursts (plus an optional discarded warm-up) and aggregate.
async fn measure_runs<F>(args: &Args, mut make: F) -> Value
where
    F: FnMut(usize) -> RpcPool,
{
    if args.warmup {
        let _ = one_burst(make(0), args.requests, args.concurrency).await;
        cooldown().await;
    }

    let mut srate = Vec::new();
    let mut rps = Vec::new();
    let mut elapsed = Vec::new();
    let mut p50 = Vec::new();
    let mut p95 = Vec::new();

    for run in 1..=args.runs {
        if run > 1 {
            cooldown().await;
        }
        let (sr, tp, el, q50, q95) = one_burst(make(run), args.requests, args.concurrency).await;
        srate.push(sr);
        rps.push(tp);
        elapsed.push(el);
        p50.push(q50);
        p95.push(q95);
    }

    let sr_lo = srate.iter().cloned().fold(f64::INFINITY, f64::min);
    let sr_hi = srate.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let tp_lo = rps.iter().cloned().fold(f64::INFINITY, f64::min);
    let tp_hi = rps.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    json!({
        "runs": args.runs,
        "success_rate": round3(median_f64(srate)),
        "success_rate_lo": round3(sr_lo),
        "success_rate_hi": round3(sr_hi),
        "throughput_rps": round1(median_f64(rps)),
        "throughput_rps_lo": round1(tp_lo),
        "throughput_rps_hi": round1(tp_hi),
        "elapsed_s": round1(median_f64(elapsed)),
        "p50_ms": median_u64(p50),
        "p95_ms": median_u64(p95),
    })
}

fn emit(args: &Args, pool_size: usize, m: Value) {
    let mut out = json!({
        "route": args.route,
        "pass": args.pass,
        "chain": args.chain,
        "pool_size": pool_size,
        "requests": args.requests,
        "concurrency": args.concurrency,
    });
    if let (Value::Object(dst), Value::Object(src)) = (&mut out, m) {
        dst.extend(src);
    }
    println!("{out}");
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    if !["spread", "chain", "capped"].contains(&args.route.as_str()) {
        eprintln!(
            "unknown route: {} (expected spread|chain|capped)",
            args.route
        );
        std::process::exit(2);
    }
    if args.route == "capped" && args.cap.is_none() {
        eprintln!("--route capped requires --cap N");
        std::process::exit(2);
    }

    match args.mode.as_str() {
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
            let m = measure_runs(&args, |_run| {
                RpcPool::with_default_transport(RpcPoolConfig::from_urls([url.clone()]))
            })
            .await;
            emit(&args, 1, m);
        }
        "pool" => {
            let eps = preset_endpoints(&args.chain);
            let size = args.pool_size.unwrap_or(eps.len()).clamp(1, eps.len());
            let m = measure_runs(&args, |run| {
                build(
                    &eps,
                    size,
                    &args.route,
                    args.cap,
                    &args.pass,
                    args.seed,
                    run,
                )
            })
            .await;
            emit(&args, size, m);
        }
        "sweep" => {
            let eps = preset_endpoints(&args.chain);
            let cap = args.max_pool.unwrap_or(eps.len()).clamp(1, eps.len());
            for size in 1..=cap {
                if size > 1 {
                    cooldown().await;
                }
                let m = measure_runs(&args, |run| {
                    build(
                        &eps,
                        size,
                        &args.route,
                        args.cap,
                        &args.pass,
                        args.seed,
                        run,
                    )
                })
                .await;
                emit(&args, size, m);
            }
        }
        other => {
            eprintln!("unknown mode: {other} (expected single|pool|sweep)");
            std::process::exit(2);
        }
    }
}
