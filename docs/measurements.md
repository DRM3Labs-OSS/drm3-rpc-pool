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

There are two different numbers here, and conflating them is where people go
wrong.

**Usable throughput (successful calls/sec) - up to ~10x.** A single free endpoint
under load fails most of its calls, so the rate that actually *lands* is a
fraction of what you attempt. A small pool routes around the failures and lands
~all of them. One live Base sweep (300 requests, concurrency 80, median of 2):

| Pool size | Success rate | Raw req/s | **Usable req/s** (raw x success) |
|-----------|--------------|-----------|----------------------------------|
| 1         | ~25%         | 196       | **~49**                          |
| 2         | ~100%        | 522       | **~522**                         |
| 3         | ~100%        | 172       | ~172                             |
| 4         | ~100%        | 184       | ~184                             |
| 5         | ~100%        | 173       | ~173                             |

That is the order-of-magnitude story: ~49 -> ~522 usable req/s going from one
endpoint to a pool. The single-endpoint success rate swings run to run (0-50%),
so the exact multiple swings with it, but the direction never does.

**Raw capacity does NOT scale linearly.** Notice raw req/s *peaks at 2 endpoints
then falls* - free public RPCs rate-limit per IP and some are flaky, so adding
slower peers drags a spread-routed pool down. ~2-3 quality endpoints is the
throughput sweet spot; past that you are buying redundancy, not speed. Pooling
ten free endpoints does NOT give you ten times the raw rate.

Raw throughput aggregates linearly only across endpoints with their own
independent capacity - your keyed providers on separate accounts. Pool those as
peers (equal `priority`) and your usable rate is roughly the sum of their limits.

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
