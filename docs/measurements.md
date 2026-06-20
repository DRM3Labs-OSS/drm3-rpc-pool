# What pooling gets you

What a pool of endpoints does for you over one hardcoded RPC URL.

## Reliability

A single endpoint that rate-limits (429) or has a bad minute takes your call with
it. A pool retries the next healthy endpoint on any 429, error, or timeout.

In load tests against free public Base endpoints (a burst of a few hundred
requests at high concurrency), a single endpoint completed 30-50% of calls; the
full pool completed 90-100% (one run: 37% on one endpoint, 91% across five). The
numbers move run to run because free endpoints are flaky; the gap does not.

## Throughput

Pooling ten free endpoints does not give you ten times the throughput. Free
public RPCs rate-limit per IP and some are flaky: a sweep of pool sizes 1 through
5 peaks at two or three endpoints, then falls as load lands on slower or failing
ones. Free public endpoints are failover and overflow, not a throughput farm.

Throughput aggregates only across endpoints with their own independent capacity -
your keyed providers on separate accounts. Pool those as peers (equal `priority`)
and your usable rate is roughly the sum of their limits.

## Cost

To cap a metered bill, put your paid provider first with a `max_in_flight` cap
and free endpoints behind it. Traffic runs on the paid key; bursts over the cap
spill onto the free endpoints. Invert it (free first, paid as the safety net) to
stay on the free tier and pay only when free capacity runs out.

## Measure your own pool

```sh
cargo run --release --example throughput -- \
  --mode sweep --max-pool 5 --route spread --runs 3 --warmup \
  --chain base --requests 500 --concurrency 120
```

Prints one JSON line per pool size with success rate, throughput, and p50/p95
latency (median of `--runs`, with a min-max band). See
[Configure routing](../README.md#configure-routing) to set it up.
