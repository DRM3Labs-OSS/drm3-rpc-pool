//! Throughput benchmark: two-pass, deterministic, pool-size focused.
//!
//! Fires `requests` calls at `concurrency` in-flight against a pool and reports
//! success-rate, throughput (req/s), and p50/p95 latency as a single JSON line
//! per (pass, pool-size). Two things make this honest rather than a single
//! lucky sample:
//!
//!   * `--runs K` repeats each measurement K times and reports the MEDIAN plus
//!     a min..max band, so one noisy public-RPC window can't define the result.
//!   * `--pass pinned|shuffled` controls endpoint ORDER. This pool is strict
//!     priority-failover (every call tries endpoint #1 first; #2 only when #1
//!     is throttled/cooling-down), so order = which provider is your primary,
//!     and the primary carries the burst. The `pinned` pass keeps the preset
//!     order fixed across runs (band = pure network noise on a fixed primary);
//!     the `shuffled` pass reshuffles the order every run with a seeded PRNG
//!     (band = order-induced spread). Comparing the two bands is the point:
//!     success rate holds either way, but throughput rides on the primary.
//!
//! Modes:
//!
//!   * `--mode single` - all load at ONE endpoint (pool size 1, no failover).
//!   * `--mode pool`   - load against the first `--pool-size` preset endpoints
//!     with health, backoff and automatic failover. The default benchmark.
//!   * `--mode sweep`  - run pool size 1..max-pool, one JSON line per size.
//!
//! Public RPCs are IP-rate-limited, so distinct pool sizes are meant to run on
//! separate machines / CI runners (separate IPs) to keep the comparison
//! apples-to-apples. K runs within one runner share an IP and are therefore
//! correlated; the band captures variance, not independent samples. See
//! `.github/workflows/benchmark.yml`.
//!
//! Usage:
//!   cargo run --release --example throughput -- \
//!       --mode pool --pool-size 2 --pass pinned   --runs 3 --warmup \
//!       --chain base --requests 1000 --concurrency 250
//!   cargo run --release --example throughput -- \
//!       --mode pool --pool-size 2 --pass shuffled --runs 3 --warmup --seed 42 \
//!       --chain base --requests 1000 --concurrency 250
//!
//! For the published chart we use `--mock`: a deterministic in-process A/B that
//! removes network noise and isolates the routing strategy. Each synthetic
//! endpoint serves `--mock-capacity` requests at once at a fixed
//! `--mock-latency-ms`; excess requests queue. With `--route chain` the burst
//! bottlenecks on one endpoint while the rest sit idle; `spread`/`capped` use
//! the whole pool.
//!
//! Flags:
//!   --mode single|pool|sweep   (default: pool; ignored under --mock)
//!   --pass pinned|shuffled     (default: pinned)
//!   --route spread|chain|capped(default: spread; chain=strict, capped needs --cap)
//!   --cap <N>                  (capped route: per-endpoint soft in-flight cap)
//!   --mock                     (controlled in-process A/B, no network)
//!   --mock-endpoints <N>       (mock: synthetic endpoint count; default: 3)
//!   --mock-capacity <C>        (mock: per-endpoint concurrency; default: 40)
//!   --mock-latency-ms <L>      (mock: per-request latency; default: 50)
//!   --runs <K>                 (median of K bursts; default: 1)
//!   --warmup                   (fire one extra burst first and discard it)
//!   --seed <N>                 (shuffled pass PRNG seed; default: 1)
//!   --chain <preset>           (default: base; any preset name/alias)
//!   --requests <N>             (default: 1000)
//!   --concurrency <N>          (default: 250)
//!   --max-pool <N>             (sweep only: cap pool size; default: all)
//!   --pool-size <N>            (run a single pool size of exactly N endpoints)
//!   --endpoint <url>           (single mode only: override the target endpoint)

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use drm3_rpc_pool::{
    presets, BackoffPolicy, NoopMetrics, RpcEndpoint, RpcPool, RpcPoolConfig, Transport,
};
use serde_json::{json, Value};

struct Args {
    mode: String,
    pass: String,
    route: String,
    cap: Option<u32>,
    mock: bool,
    mock_endpoints: usize,
    mock_capacity: usize,
    mock_latency_ms: u64,
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
    let mut mode = "pool".to_string();
    let mut pass = "pinned".to_string();
    let mut route = "spread".to_string();
    let mut cap: Option<u32> = None;
    let mut mock = false;
    let mut mock_endpoints = 3usize;
    let mut mock_capacity = 40usize;
    let mut mock_latency_ms = 50u64;
    let mut runs = 1usize;
    let mut warmup = false;
    let mut seed = 1u64;
    let mut chain = "base".to_string();
    let mut requests = 1000usize;
    let mut concurrency = 250usize;
    let mut max_pool: Option<usize> = None;
    let mut pool_size: Option<usize> = None;
    let mut endpoint: Option<String> = None;

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--mode" => mode = it.next().unwrap_or(mode),
            "--pass" => pass = it.next().unwrap_or(pass),
            "--route" => route = it.next().unwrap_or(route),
            "--cap" => cap = it.next().and_then(|v| v.parse().ok()),
            "--mock" => mock = true,
            "--mock-endpoints" => {
                mock_endpoints = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(mock_endpoints)
                    .max(1)
            }
            "--mock-capacity" => {
                mock_capacity = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(mock_capacity)
                    .max(1)
            }
            "--mock-latency-ms" => {
                mock_latency_ms = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(mock_latency_ms)
            }
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
                    "throughput --mode single|pool|sweep --pass pinned|shuffled \
                     --route spread|chain|capped [--cap N] --runs K --warmup \
                     --seed N --chain <preset> --requests N --concurrency N \
                     [--max-pool N] [--pool-size N] [--endpoint URL]"
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
        pass,
        route,
        cap,
        mock,
        mock_endpoints,
        mock_capacity,
        mock_latency_ms,
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

/// Fisher-Yates permutation of `eps` in place. Pure reordering; `priority` is
/// assigned afterwards by the routing policy (see `build_pass`).
fn permute(eps: &mut [RpcEndpoint], rng: &mut SplitMix64) {
    let len = eps.len();
    for i in (1..len).rev() {
        let j = rng.below((i + 1) as u64) as usize;
        eps.swap(i, j);
    }
}

fn order_tags(eps: &[RpcEndpoint]) -> Vec<String> {
    eps.iter().map(|e| e.tag().to_string()).collect()
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

/// One burst: fire `requests` calls bounded to `concurrency` in flight; return
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
/// into the next on a shared-IP run (local, or K runs on one runner).
async fn cooldown() {
    let secs: u64 = std::env::var("SWEEP_COOLDOWN_S")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    if secs > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
    }
}

/// Run `runs` bursts (plus an optional discarded warm-up) against a pool built
/// fresh per run by `build`, which receives the 1-based run index and returns
/// `(pool, order_tags)`. Returns the aggregated JSON report fields.
async fn measure_runs<F>(
    runs: usize,
    warmup: bool,
    requests: usize,
    concurrency: usize,
    mut build: F,
) -> Value
where
    F: FnMut(usize) -> (RpcPool, Vec<String>),
{
    if warmup {
        let (pool, _) = build(0);
        let _ = one_burst(pool, requests, concurrency).await;
        cooldown().await;
    }

    let mut srate = Vec::with_capacity(runs);
    let mut rps = Vec::with_capacity(runs);
    let mut elapsed = Vec::with_capacity(runs);
    let mut p50 = Vec::with_capacity(runs);
    let mut p95 = Vec::with_capacity(runs);
    let mut orders: Vec<Vec<String>> = Vec::with_capacity(runs);
    let mut raw: Vec<Value> = Vec::with_capacity(runs);

    for run in 1..=runs {
        if run > 1 {
            cooldown().await;
        }
        let (pool, order) = build(run);
        let (sr, tp, el, q50, q95) = one_burst(pool, requests, concurrency).await;
        srate.push(sr);
        rps.push(tp);
        elapsed.push(el);
        p50.push(q50);
        p95.push(q95);
        orders.push(order.clone());
        raw.push(json!({
            "order": order,
            "success_rate": round3(sr),
            "throughput_rps": round1(tp),
            "elapsed_s": round1(el),
            "p50_ms": q50,
            "p95_ms": q95,
        }));
    }

    let sr_lo = srate.iter().cloned().fold(f64::INFINITY, f64::min);
    let sr_hi = srate.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let tp_lo = rps.iter().cloned().fold(f64::INFINITY, f64::min);
    let tp_hi = rps.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    json!({
        "runs": runs,
        "success_rate": round3(median_f64(srate)),
        "success_rate_lo": round3(sr_lo),
        "success_rate_hi": round3(sr_hi),
        "throughput_rps": round1(median_f64(rps)),
        "throughput_rps_lo": round1(tp_lo),
        "throughput_rps_hi": round1(tp_hi),
        "elapsed_s": round1(median_f64(elapsed)),
        "p50_ms": median_u64(p50),
        "p95_ms": median_u64(p95),
        "orders": orders,
        "runs_raw": raw,
    })
}

/// Merge identity fields onto an aggregated report and print it as one JSON line.
#[allow(clippy::too_many_arguments)]
fn emit(
    mode: &str,
    pass: &str,
    route: &str,
    chain: &str,
    seed: u64,
    requests: usize,
    concurrency: usize,
    pool_size: usize,
    m: Value,
) {
    let mut out = json!({
        "mode": mode,
        "pass": pass,
        "route": route,
        "chain": chain,
        "seed": seed,
        "pool_size": pool_size,
        "requests": requests,
        "concurrency": concurrency,
    });
    if let (Value::Object(dst), Value::Object(src)) = (&mut out, m) {
        dst.extend(src);
    }
    println!("{out}");
}

/// Build a pool of the first `size` preset endpoints, applying the pass and
/// routing policies. `pass`: `pinned` keeps preset order every run, `shuffled`
/// permutes per run from a run-derived seed (so the band reflects order
/// sensitivity). `route`: `spread` makes every endpoint an equal-priority peer
/// (priority 0 → the pool spreads concurrent load by least in-flight - the
/// library default), `chain` assigns distinct priorities by position (strict
/// priority-failover - the pre-optimization behavior, kept here so the chart
/// can A/B the two).
fn build_pass(
    eps: &[RpcEndpoint],
    size: usize,
    pass: &str,
    route: &str,
    cap: Option<u32>,
    seed: u64,
    run: usize,
) -> (RpcPool, Vec<String>) {
    let mut subset: Vec<RpcEndpoint> = eps.iter().take(size).cloned().collect();
    if pass == "shuffled" {
        // run 0 is the warm-up; give it its own permutation too.
        let mut rng = SplitMix64(seed ^ (run as u64).wrapping_mul(0x100_0000_01B3));
        permute(&mut subset, &mut rng);
    }
    // `chain`/`capped` keep a ranked order (priority = position); `spread`
    // flattens to equal-priority peers. `capped` additionally sets a soft
    // concurrency cap so the primary spills to failover once it fills.
    for (i, ep) in subset.iter_mut().enumerate() {
        ep.priority = if route == "spread" { 0 } else { i as u32 };
        ep.max_in_flight = if route == "capped" { cap } else { None };
    }
    let order = order_tags(&subset);
    let pool = RpcPool::with_default_transport(RpcPoolConfig {
        endpoints: subset,
        ..RpcPoolConfig::default()
    });
    (pool, order)
}

// ── Controlled (in-process) transport ───────────────────────────────────
// Models each endpoint as a server with a fixed per-request latency and a hard
// concurrency limit: up to `capacity` requests are served at once, excess
// requests QUEUE for a slot - they are not rejected. This is the honest way to
// isolate the routing mechanism from real-network noise: with a saturated but
// healthy endpoint, strict-chain routing bottlenecks on the primary's slots
// while the other endpoints sit idle (chain only fails over on an *error*, and
// queuing is not an error), whereas least-loaded (`spread`) and `capped`
// routing fill every endpoint. Deterministic, no network.

struct MockEndpoint {
    slots: tokio::sync::Semaphore,
    latency: Duration,
}

struct MockTransport {
    endpoints: HashMap<String, MockEndpoint>,
}

impl MockTransport {
    fn new(urls: &[String], capacity: usize, latency_ms: u64) -> Arc<Self> {
        let endpoints = urls
            .iter()
            .map(|u| {
                (
                    u.clone(),
                    MockEndpoint {
                        slots: tokio::sync::Semaphore::new(capacity),
                        latency: Duration::from_millis(latency_ms),
                    },
                )
            })
            .collect();
        Arc::new(Self { endpoints })
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn post_json(&self, url: &str, _body: Vec<u8>) -> Result<Vec<u8>, String> {
        let ep = self
            .endpoints
            .get(url)
            .ok_or_else(|| format!("mock: unknown url {url}"))?;
        // Queue for a serving slot, then take the fixed latency. Holding the
        // permit across the sleep is what bounds the endpoint's concurrency.
        let _permit = ep.slots.acquire().await.map_err(|e| e.to_string())?;
        tokio::time::sleep(ep.latency).await;
        Ok(serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":"0x1"})).unwrap())
    }
}

/// Build a controlled pool of `n` synthetic endpoints (each capacity/latency
/// from the mock args), applying the same pass + route policy as `build_pass`.
fn build_controlled(args: &Args, route: &str, run: usize) -> (RpcPool, Vec<String>) {
    let n = args.mock_endpoints;
    let urls: Vec<String> = (0..n).map(|i| format!("mock://ep{i}")).collect();
    let mut eps: Vec<RpcEndpoint> = urls
        .iter()
        .enumerate()
        .map(|(i, u)| RpcEndpoint {
            label: Some(format!("ep{i}")),
            ..RpcEndpoint::new(u.clone())
        })
        .collect();
    if args.pass == "shuffled" {
        let mut rng = SplitMix64(args.seed ^ (run as u64).wrapping_mul(0x100_0000_01B3));
        permute(&mut eps, &mut rng);
    }
    for (i, ep) in eps.iter_mut().enumerate() {
        ep.priority = if route == "spread" { 0 } else { i as u32 };
        ep.max_in_flight = if route == "capped" { args.cap } else { None };
    }
    let order = order_tags(&eps);
    let transport = MockTransport::new(&urls, args.mock_capacity, args.mock_latency_ms);
    let pool = RpcPool::new(
        RpcPoolConfig {
            endpoints: eps,
            ..RpcPoolConfig::default()
        },
        transport,
        Arc::new(NoopMetrics),
        BackoffPolicy::default(),
    )
    .expect("valid mock config");
    (pool, order)
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    let pass = args.pass.as_str();
    if pass != "pinned" && pass != "shuffled" {
        eprintln!("unknown pass: {pass} (expected pinned|shuffled)");
        std::process::exit(2);
    }
    let route = args.route.as_str();
    if route != "spread" && route != "chain" && route != "capped" {
        eprintln!("unknown route: {route} (expected spread|chain|capped)");
        std::process::exit(2);
    }
    if route == "capped" && args.cap.is_none() {
        eprintln!("--route capped requires --cap N (max in-flight per endpoint)");
        std::process::exit(2);
    }

    // Controlled mode short-circuits the preset/real-network path: a fully
    // deterministic in-process A/B of the routing strategy, no network noise.
    if args.mock {
        let m = measure_runs(
            args.runs,
            args.warmup,
            args.requests,
            args.concurrency,
            |run| build_controlled(&args, route, run),
        )
        .await;
        let mut out = json!({
            "mode": "controlled",
            "pass": pass,
            "route": route,
            "cap": args.cap,
            "mock_endpoints": args.mock_endpoints,
            "mock_capacity": args.mock_capacity,
            "mock_latency_ms": args.mock_latency_ms,
            "seed": args.seed,
            "pool_size": args.mock_endpoints,
            "requests": args.requests,
            "concurrency": args.concurrency,
        });
        if let (Value::Object(dst), Value::Object(src)) = (&mut out, m) {
            dst.extend(src);
        }
        println!("{out}");
        return;
    }

    match args.mode.as_str() {
        // Whole-preset pool, full failover. With `--pool-size N`, take exactly
        // the first N preset endpoints (lets CI run one pool size per runner).
        "pool" => {
            let eps = preset_endpoints(&args.chain);
            let size = args.pool_size.unwrap_or(eps.len()).clamp(1, eps.len());
            let m = measure_runs(
                args.runs,
                args.warmup,
                args.requests,
                args.concurrency,
                |run| build_pass(&eps, size, pass, route, args.cap, args.seed, run),
            )
            .await;
            emit(
                "pool",
                pass,
                route,
                &args.chain,
                args.seed,
                args.requests,
                args.concurrency,
                size,
                m,
            );
        }

        // One endpoint only (pool size 1): the override, else the preset's first.
        // Order is moot at size 1, so the pass label is forced to "pinned".
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
            let m = measure_runs(
                args.runs,
                args.warmup,
                args.requests,
                args.concurrency,
                |_run| {
                    let pool =
                        RpcPool::with_default_transport(RpcPoolConfig::from_urls([url.clone()]));
                    (pool, vec![url.clone()])
                },
            )
            .await;
            emit(
                "single",
                "pinned",
                route,
                &args.chain,
                args.seed,
                args.requests,
                args.concurrency,
                1,
                m,
            );
        }

        // Pool-size sweep: 1..max-pool. One JSON line per size, same pass policy.
        "sweep" => {
            let eps = preset_endpoints(&args.chain);
            let cap = args.max_pool.unwrap_or(eps.len()).clamp(1, eps.len());
            for size in 1..=cap {
                if size > 1 {
                    cooldown().await;
                }
                let mode = if size == 1 { "single" } else { "pool" };
                let pass_for = if size == 1 { "pinned" } else { pass };
                let m = measure_runs(
                    args.runs,
                    args.warmup,
                    args.requests,
                    args.concurrency,
                    |run| build_pass(&eps, size, pass_for, route, args.cap, args.seed, run),
                )
                .await;
                emit(
                    mode,
                    pass_for,
                    route,
                    &args.chain,
                    args.seed,
                    args.requests,
                    args.concurrency,
                    size,
                    m,
                );
            }
        }

        other => {
            eprintln!("unknown mode: {other} (expected single|pool|sweep)");
            std::process::exit(2);
        }
    }
}
