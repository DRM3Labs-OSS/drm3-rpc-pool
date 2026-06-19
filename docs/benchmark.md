# Benchmark: routing under load

The one thing worth measuring here is the **routing decision**: when a burst is
bigger than any single endpoint can absorb, does the pool use the *rest* of its
endpoints, or pile everything on the first one?

That is hard to measure on real public RPCs, because their throughput is
dominated by which endpoint happens to be healthy in the moment, not by how you
route. So the headline benchmark is **controlled**: a deterministic, in-process
A/B (`examples/throughput.rs --mock`) with no network. Each synthetic endpoint
serves a fixed number of requests at once at a fixed latency; excess requests
queue (a saturated-but-healthy endpoint, *not* an error). The only variable
across the three bars is the routing strategy.

<!-- BENCHMARK:START -->
**Controlled benchmark - 3 endpoints, each capacity 40 @ 50ms, 600 requests @ concurrency 200, median of 3 runs**

_Method: a deterministic, in-process A/B (no network) that isolates the routing strategy. Each synthetic endpoint serves 40 requests at once at a fixed 50ms; excess requests queue (a saturated-but-healthy endpoint, not an error). One endpoint tops out at **800 req/s**; the whole 3-endpoint pool can do **2400 req/s**. The only variable is how the pool routes. This is a lab benchmark by design - it removes public-RPC noise so the mechanism is legible; field numbers against free public endpoints are far noisier and dominated by which endpoint is healthy in the moment._

![Strict failover bottlenecks on one endpoint; load-aware routing uses the whole pool](../assets/benchmark.svg)

| Routing | Throughput (req/s) | p50 | p95 | Success | What happens |
|---------|-------------------:|----:|----:|--------:|--------------|
| chain (strict failover) | 770.7 (770.4-774.7) | 259 ms | 260 ms | 100% | rides one endpoint; the rest of the pool is idle |
| spread (least in-flight) | 1919.1 (1913-1924.6) | 103 ms | 104 ms | 100% | fills every peer evenly |
| capped (cap=40) | 1650.1 (1648.2-1650.3) | 52 ms | 156 ms | 100% | rides the primary to its cap, then spills |

#### What this proves

- **Strict failover leaves capacity on the table.** `chain` sends every request to endpoint #1 first and only fails over on an *error*. A saturated-but-healthy endpoint never errors, so the burst queues on one endpoint while the other 2 sit idle - throughput pins at the single-endpoint ceiling (~800 req/s).
- **Load-aware routing uses the whole pool.** `spread` (least in-flight across equal-priority peers) and `capped` (ride a preferred primary up to `max_in_flight`, then spill) both put work on every endpoint, ~2.5× the throughput of `chain` - and `capped` also gives the best p50 because the primary's first cap-worth of requests never queue.
- **Pick by goal.** Homogeneous peers and want max throughput → `spread` (equal `priority`). Want a keyed/paid primary to carry load but not melt down under a burst → `capped` (lower `priority` + `max_in_flight`). Want strict ordering and accept the bottleneck → leave it `chain` (distinct priorities, no cap), the default.
- **Implementation:** dispatch orders candidates by `(saturated, priority, in-flight, index)` in `src/pool/mod.rs`; every endpoint tracks live in-flight load, and a soft `max_in_flight` cap marks an endpoint saturated so traffic spills to peers before piling on.

_Auto-generated 2026-06-19 19:53:23 UTC. Deterministic controlled benchmark; reproduce with the command in [Reproduce](#reproduce) below._
<!-- BENCHMARK:END -->

## Why a model, not field numbers

The synthetic endpoints are not a performance claim about real providers. They
exist to isolate one variable. A field A/B of the same strategies is pure noise:
run to run it says whatever the public endpoints were doing that second. Real
numbers we measured against three free Base endpoints, 3-run medians:

| concurrency | chain | spread |
|------------:|-------|--------|
| 30  | 100% / 336 req/s | 50% / 39 req/s (one run: an endpoint was simply down) |
| 80  | 50% / 43 req/s   | 100% / 65 req/s |
| 150 | 94% / 99 req/s   | 28% / 39 req/s |

There is no winner in that table, only weather. That is exactly why the
mechanism is shown in isolation instead: the model removes the weather so the
routing decision is legible. What it shows is narrow and honest - strict
failover leaves `N-1` endpoints idle; load-aware routing uses them - and nothing
about absolute req/s carries over to your providers.

## What the model assumes

- Each endpoint serves `--mock-capacity` requests concurrently at a fixed
  `--mock-latency-ms`; beyond that, requests queue. This models an endpoint that
  *slows down* under load rather than erroring.
- A queued (slow) endpoint is not a failure, so `chain` routing never fails over
  off it - which is the whole point: a saturated-but-healthy primary silently
  bottlenecks a strict chain.
- Real endpoints also reject (429) and vary in capacity and latency. The model
  deliberately omits that; it is a lower bound on the *mechanism*, not a forecast.

## Reproduce

Controlled (deterministic - same numbers every run):

```sh
for route in chain spread capped; do
  cargo run --release --example throughput -- \
    --mock --mock-endpoints 3 --mock-capacity 40 --mock-latency-ms 50 \
    --route "$route" --cap 40 --runs 3 --requests 600 --concurrency 200
done
```

Field (real endpoints, honest but noisy - drop `--mock`):

```sh
cargo run --release --example throughput -- \
  --mode pool --pool-size 3 --route spread --pass shuffled --seed 42 \
  --runs 5 --warmup --chain base --requests 600 --concurrency 150
```

`--runs K` reports the median of K bursts with a min-max band; `--warmup`
discards a priming burst; `--pass shuffled` varies endpoint order across runs so
the band captures order sensitivity. The CI workflow
([`benchmark.yml`](../.github/workflows/benchmark.yml)) runs the controlled A/B
and regenerates the chart above.

## The takeaway for your config

This benchmark is the *why* behind one config choice: give interchangeable
endpoints the **same `priority`** so the pool spreads load across them, instead
of a strict ranked chain that rides one until it errors. See
[Configure routing](../README.md#configure-routing) in the README.
