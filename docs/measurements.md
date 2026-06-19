# What pooling gets you (measured)

Plain answers to one question: what does putting a pool behind a single call
actually buy you, versus one hardcoded RPC URL? Here is what we measured and
where the honest line is.

## Reliability: yes, and it shows up immediately

A single endpoint that rate-limits (429) or has a bad minute takes your call
with it. A pool retries the next healthy endpoint on any 429, error, or timeout.

In our own load tests against free public Base endpoints (500 requests at
concurrency 120), a single endpoint completed only **30-50% of calls** during a
burst. The moment a second healthy endpoint was in the pool, completion went to
**~100%**. Exact numbers swing run to run because free endpoints are flaky, but
the direction is steady: one endpoint drops calls under load, a pool of two or
more completes them. This is the reason to use it.

## More throughput from free tiers: no, not from free public RPCs

It is tempting to assume that pooling ten free endpoints gives you ten times the
throughput. It does not. Free public RPCs rate-limit per IP and some are flaky,
so spreading a heavy burst across them does not scale: we swept pool sizes 1
through 5 and throughput peaked at two or three endpoints, then fell as load
landed on slower or failing ones. Treat free public endpoints as failover and
overflow, not a throughput farm.

You get a real throughput multiplier only when the endpoints have their own
**independent capacity** - your own keyed providers on separate accounts or
plans. Pool those as peers (equal `priority`) and your usable rate is roughly
the sum of their limits. Point the load test below at your keyed endpoints to
measure your own setup.

## Offsetting cost on a high-call workload: yes

If you make a lot of calls and want to cap a metered bill, put your paid
provider first with a `max_in_flight` cap and free endpoints behind it. Normal
traffic runs on the paid key; bursts over the cap spill onto the free endpoints
instead of onto your bill. Invert it (free first, paid as the safety net) if you
would rather stay on the free tier and only pay when free capacity runs out.

## Measure your own pool

```sh
cargo run --release --example throughput -- \
  --mode sweep --max-pool 5 --route spread --runs 3 --warmup \
  --chain base --requests 500 --concurrency 120
```

Prints one JSON line per pool size with success rate, throughput, and p50/p95
latency (median of `--runs`, with a min-max band). Against free public endpoints
expect noisy throughput and a clear success-rate gain; against endpoints with
real capacity, both gains are clear. See
[Configure routing](../README.md#configure-routing) for how to set it up.
