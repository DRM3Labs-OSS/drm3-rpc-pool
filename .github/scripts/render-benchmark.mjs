#!/usr/bin/env node
// Render the routing-strategy benchmark into assets/benchmark.svg and the
// README's <!-- BENCHMARK:START --> .. <!-- BENCHMARK:END --> block.
//
// Usage: node render-benchmark.mjs <results.jsonl> [more.jsonl ...]
//
// Each input line is one JSON report from `examples/throughput.rs --mock`
// (controlled, deterministic). Records carry `route` (chain|spread|capped),
// the median `throughput_rps`(+_lo/_hi band), `p50_ms`/`p95_ms`,
// `success_rate`, and the mock parameters. We render a throughput bar per
// routing strategy so the mechanism is visual: strict `chain` bottlenecks on
// one endpoint while the rest of the pool sits idle; load-aware `spread`/`capped`
// use the whole pool. Pure Node stdlib, no dependencies.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

const inputs = process.argv.slice(2);
if (inputs.length === 0) {
  console.error("usage: render-benchmark.mjs <results.jsonl> [more.jsonl ...]");
  process.exit(2);
}

function loadAll(path) {
  return readFileSync(path, "utf8")
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.startsWith("{"))
    .map((l) => JSON.parse(l));
}

// Dedupe by route (last write wins) and order chain → spread → capped.
const ORDER = ["chain", "spread", "capped"];
const byRoute = new Map();
for (const path of inputs) {
  for (const rec of loadAll(path)) {
    if (typeof rec.route !== "string") {
      console.error(`record missing route in ${path}: ${JSON.stringify(rec)}`);
      process.exit(1);
    }
    byRoute.set(rec.route, rec);
  }
}
const rows = [...byRoute.values()].sort(
  (a, b) => ORDER.indexOf(a.route) - ORDER.indexOf(b.route),
);
if (rows.length === 0) {
  console.error("no benchmark records found");
  process.exit(1);
}

const meta = rows[0];
const lo = (r, k) => (r[`${k}_lo`] == null ? r[k] : r[`${k}_lo`]);
const hi = (r, k) => (r[`${k}_hi`] == null ? r[k] : r[`${k}_hi`]);
const fmt = (v) => (v == null ? "n/a" : String(v));
const now = new Date().toISOString().replace("T", " ").replace(/\..+/, " UTC");
const n = meta.mock_endpoints;
const cap = meta.mock_capacity;
const latMs = meta.mock_latency_ms;
const latS = latMs / 1000;
const oneCeil = Math.round(cap / latS); // single-endpoint ceiling, req/s
const fullCeil = Math.round((n * cap) / latS); // whole-pool ceiling, req/s

const cappedCap = byRoute.get("capped")?.cap;
const ROUTE_LABEL = {
  chain: "chain (strict failover)",
  spread: "spread (least in-flight)",
  capped: `capped (cap=${cappedCap ?? "?"})`,
};
const ROUTE_NOTE = {
  chain: "rides one endpoint; the rest of the pool is idle",
  spread: "fills every peer evenly",
  capped: "rides the primary to its cap, then spills",
};

// ── Markdown block ─────────────────────────────────────────────────────
const header = `**Controlled benchmark - ${n} endpoints, each capacity ${cap} @ ${latMs}ms, ${meta.requests} requests @ concurrency ${meta.concurrency}, median of ${meta.runs} runs**`;

const method = `_Method: a deterministic, in-process A/B (no network) that isolates the routing strategy. Each synthetic endpoint serves ${cap} requests at once at a fixed ${latMs}ms; excess requests queue (a saturated-but-healthy endpoint, not an error). One endpoint tops out at **${oneCeil} req/s**; the whole ${n}-endpoint pool can do **${fullCeil} req/s**. The only variable is how the pool routes. This is a lab benchmark by design - it removes public-RPC noise so the mechanism is legible; field numbers against free public endpoints are far noisier and dominated by which endpoint is healthy in the moment._`;

const table =
  `| Routing | Throughput (req/s) | p50 | p95 | Success | What happens |\n` +
  `|---------|-------------------:|----:|----:|--------:|--------------|\n` +
  rows
    .map((r) => {
      const band =
        lo(r, "throughput_rps") === hi(r, "throughput_rps")
          ? fmt(r.throughput_rps)
          : `${fmt(r.throughput_rps)} (${fmt(lo(r, "throughput_rps"))}-${fmt(hi(r, "throughput_rps"))})`;
      return `| ${ROUTE_LABEL[r.route] ?? r.route} | ${band} | ${fmt(r.p50_ms)} ms | ${fmt(r.p95_ms)} ms | ${(r.success_rate * 100).toFixed(0)}% | ${ROUTE_NOTE[r.route] ?? ""} |`;
    })
    .join("\n") +
  "\n";

const best = rows.reduce((a, b) => (b.throughput_rps > a.throughput_rps ? b : a));
const chain = byRoute.get("chain");
const speedup =
  chain && chain.throughput_rps
    ? (best.throughput_rps / chain.throughput_rps).toFixed(1)
    : null;

const block =
  `${header}\n\n` +
  `${method}\n\n` +
  `![Strict failover bottlenecks on one endpoint; load-aware routing uses the whole pool](../assets/benchmark.svg)\n\n` +
  table +
  `\n#### What this proves\n\n` +
  `- **Strict failover leaves capacity on the table.** \`chain\` sends every request to endpoint #1 first and only fails over on an *error*. A saturated-but-healthy endpoint never errors, so the burst queues on one endpoint while the other ${n - 1} sit idle - throughput pins at the single-endpoint ceiling (~${oneCeil} req/s).\n` +
  `- **Load-aware routing uses the whole pool.** \`spread\` (least in-flight across equal-priority peers) and \`capped\` (ride a preferred primary up to \`max_in_flight\`, then spill) both put work on every endpoint${speedup ? `, ~${speedup}× the throughput of \`chain\`` : ""} - and \`capped\` also gives the best p50 because the primary's first cap-worth of requests never queue.\n` +
  `- **Pick by goal.** Homogeneous peers and want max throughput → \`spread\` (equal \`priority\`). Want a keyed/paid primary to carry load but not melt down under a burst → \`capped\` (lower \`priority\` + \`max_in_flight\`). Want strict ordering and accept the bottleneck → leave it \`chain\` (distinct priorities, no cap), the default.\n` +
  `- **Implementation:** dispatch orders candidates by \`(saturated, priority, in-flight, index)\` in \`src/pool/mod.rs\`; every endpoint tracks live in-flight load, and a soft \`max_in_flight\` cap marks an endpoint saturated so traffic spills to peers before piling on.\n\n` +
  `_Auto-generated ${now}. Deterministic controlled benchmark; reproduce with the command in [Reproduce](#reproduce) below._\n`;

// ── docs/benchmark.md rewrite ──────────────────────────────────────────
const docPath = join(repoRoot, "docs", "benchmark.md");
let doc = readFileSync(docPath, "utf8");
const START = "<!-- BENCHMARK:START -->";
const END = "<!-- BENCHMARK:END -->";
const startIdx = doc.indexOf(START);
const endIdx = doc.indexOf(END);
if (startIdx === -1 || endIdx === -1) {
  console.error("docs/benchmark.md is missing the BENCHMARK markers");
  process.exit(1);
}
doc = doc.slice(0, startIdx + START.length) + "\n" + block + doc.slice(endIdx);
writeFileSync(docPath, doc);

// ── SVG bar chart ──────────────────────────────────────────────────────
const esc = (s) =>
  String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");

const FONT = "ui-sans-serif,system-ui,Segoe UI,Helvetica,Arial,sans-serif";
const C = {
  bg: "#0d1117",
  panel: "#161b22",
  border: "#30363d",
  grid: "#21262d",
  axis: "#6e7681",
  text: "#e6edf3",
  sub: "#8b949e",
  accent: "#58a6ff",
  chain: "#f85149",
  chainTop: "#ff7b72",
  good: "#3fb950",
  goodTop: "#56d364",
  whisker: "#d29922",
  ceil: "#8b949e",
};

const N = rows.length;
const W = Math.max(720, 150 + N * 150);

const TITLE_Y = 38;
const PILL_TOP = 56;
const PILL_H = 38;
const CHART_TOP = PILL_TOP + PILL_H + 34;
const CHART_H = 250;
const CHART_BOTTOM = CHART_TOP + CHART_H;
const XLABEL_Y = CHART_BOTTOM + 26;
const XSUB_Y = XLABEL_Y + 17;
const CAPTION_Y = XSUB_Y + 30;

const PAD_L = 64;
const PAD_R = 28;
const plotW = W - PAD_L - PAD_R;

// Y scale: 0..fullCeil, with a little headroom.
const yMax = fullCeil * 1.08;
const yFor = (rps) => CHART_BOTTOM - (rps / yMax) * CHART_H;

// Gridlines + y-axis (req/s).
let grid = "";
const steps = 4;
for (let i = 0; i <= steps; i++) {
  const v = (yMax / steps) * i;
  const y = CHART_BOTTOM - (v / yMax) * CHART_H;
  grid +=
    `<line x1="${PAD_L}" y1="${y.toFixed(1)}" x2="${(W - PAD_R).toFixed(1)}" y2="${y.toFixed(1)}" stroke="${C.grid}" stroke-width="1"/>` +
    `<text x="${(PAD_L - 10).toFixed(1)}" y="${(y + 4).toFixed(1)}" fill="${C.axis}" font-family="${FONT}" font-size="11" text-anchor="end">${Math.round(v)}</text>`;
}

// Reference lines: single-endpoint and full-pool ceilings.
function refLine(v, label) {
  const y = yFor(v);
  return (
    `<line x1="${PAD_L}" y1="${y.toFixed(1)}" x2="${(W - PAD_R).toFixed(1)}" y2="${y.toFixed(1)}" stroke="${C.ceil}" stroke-width="1" stroke-dasharray="5 4" opacity="0.7"/>` +
    `<text x="${(W - PAD_R).toFixed(1)}" y="${(y - 5).toFixed(1)}" fill="${C.ceil}" font-family="${FONT}" font-size="10.5" text-anchor="end">${esc(label)}</text>`
  );
}
const refs = refLine(oneCeil, `1-endpoint ceiling ${oneCeil}`) + refLine(fullCeil, `full-pool ceiling ${fullCeil}`);

// Bars.
const slotW = plotW / N;
const barW = Math.min(96, slotW * 0.5);
let defs = "";
let bars = "";
rows.forEach((r, i) => {
  const cx = PAD_L + slotW * i + slotW / 2;
  const x = cx - barW / 2;
  const top = yFor(r.throughput_rps);
  const isChain = r.route === "chain";
  const gid = `g${i}`;
  defs +=
    `<linearGradient id="${gid}" x1="0" y1="0" x2="0" y2="1">` +
    `<stop offset="0" stop-color="${isChain ? C.chainTop : C.goodTop}"/>` +
    `<stop offset="1" stop-color="${isChain ? C.chain : C.good}"/></linearGradient>`;
  bars +=
    `<rect x="${x.toFixed(1)}" y="${top.toFixed(1)}" width="${barW.toFixed(1)}" height="${(CHART_BOTTOM - top).toFixed(1)}" rx="7" fill="url(#${gid})"/>`;

  // Min-max whisker.
  const yLo = yFor(lo(r, "throughput_rps"));
  const yHi = yFor(hi(r, "throughput_rps"));
  if (yLo - yHi > 1.5) {
    const wx = cx;
    bars +=
      `<line x1="${wx}" y1="${yHi.toFixed(1)}" x2="${wx}" y2="${yLo.toFixed(1)}" stroke="${C.whisker}" stroke-width="2"/>` +
      `<line x1="${wx - 6}" y1="${yHi.toFixed(1)}" x2="${wx + 6}" y2="${yHi.toFixed(1)}" stroke="${C.whisker}" stroke-width="2"/>` +
      `<line x1="${wx - 6}" y1="${yLo.toFixed(1)}" x2="${wx + 6}" y2="${yLo.toFixed(1)}" stroke="${C.whisker}" stroke-width="2"/>`;
  }

  // Value label above bar: throughput + p50.
  bars +=
    `<text x="${cx.toFixed(1)}" y="${(top - 26).toFixed(1)}" fill="${C.text}" font-family="${FONT}" font-size="18" font-weight="800" text-anchor="middle">${fmt(r.throughput_rps)}</text>` +
    `<text x="${cx.toFixed(1)}" y="${(top - 10).toFixed(1)}" fill="${C.sub}" font-family="${FONT}" font-size="11.5" text-anchor="middle">req/s · p50 ${fmt(r.p50_ms)}ms</text>`;

  // X labels.
  bars +=
    `<text x="${cx.toFixed(1)}" y="${XLABEL_Y}" fill="${C.text}" font-family="${FONT}" font-size="14" font-weight="700" text-anchor="middle">${esc((ROUTE_LABEL[r.route] ?? r.route).split(" ")[0])}</text>` +
    `<text x="${cx.toFixed(1)}" y="${XSUB_Y}" fill="${C.sub}" font-family="${FONT}" font-size="11" text-anchor="middle">${esc(ROUTE_NOTE[r.route] ?? "")}</text>`;
});

// Run-parameter pill.
const pillSegs = [
  `${n} endpoints`,
  `cap ${cap} @ ${latMs}ms`,
  `${meta.requests} reqs @ conc ${meta.concurrency}`,
  `median of ${meta.runs}`,
];
let pill = "";
{
  const padX = 18;
  const gap = 14;
  const dotR = 3;
  const fontSize = 14;
  const charW = fontSize * 0.6;
  const segW = pillSegs.map((s) => s.length * charW);
  const innerW = segW.reduce((a, b) => a + b, 0) + (pillSegs.length - 1) * (gap + dotR * 2 + gap);
  const pillW = innerW + padX * 2;
  const px = (W - pillW) / 2;
  pill += `<rect x="${px.toFixed(1)}" y="${PILL_TOP}" width="${pillW.toFixed(1)}" height="${PILL_H}" rx="${PILL_H / 2}" fill="${C.panel}" stroke="${C.border}" stroke-width="1"/>`;
  let tx = px + padX;
  const ty = PILL_TOP + PILL_H / 2 + 1;
  pillSegs.forEach((s, i) => {
    pill += `<text x="${tx.toFixed(1)}" y="${ty.toFixed(1)}" fill="${C.text}" font-family="${FONT}" font-size="${fontSize}" font-weight="700" dominant-baseline="middle">${esc(s)}</text>`;
    tx += segW[i];
    if (i < pillSegs.length - 1) {
      tx += gap + dotR;
      pill += `<circle cx="${tx.toFixed(1)}" cy="${ty.toFixed(1)}" r="${dotR}" fill="${C.accent}"/>`;
      tx += dotR + gap;
    }
  });
}

function wrapCentered(text, cx, y0, { size, lineH, fill, weight, pad = 32 }) {
  const maxChars = Math.max(8, Math.floor((W - 2 * pad) / (size * 0.55)));
  const words = text.split(" ");
  const lines = [];
  let cur = "";
  for (const w of words) {
    const next = cur ? `${cur} ${w}` : w;
    if (next.length > maxChars && cur) {
      lines.push(cur);
      cur = w;
    } else {
      cur = next;
    }
  }
  if (cur) lines.push(cur);
  const wt = weight ? ` font-weight="${weight}"` : "";
  const svg = lines
    .map(
      (ln, i) =>
        `<text x="${cx.toFixed(1)}" y="${(y0 + i * lineH).toFixed(1)}" fill="${fill}" font-family="${FONT}" font-size="${size}"${wt} text-anchor="middle">${esc(ln)}</text>`,
    )
    .join("\n  ");
  return { svg, nextY: y0 + lines.length * lineH };
}

const caption = `Deterministic in-process A/B: ${n} endpoints, each serving ${cap} at a time at ${latMs}ms; excess queues. Strict chain pins at the single-endpoint ceiling (${oneCeil} req/s) with the rest of the pool idle; load-aware routing uses the whole pool. Whiskers are min-max over ${meta.runs} runs (tight = deterministic). Lab benchmark; field results on free public RPCs are noisier. ${now}.`;
const cap_ = wrapCentered(caption, W / 2, CAPTION_Y, {
  size: 12,
  lineH: 16,
  fill: C.sub,
  weight: "600",
});
const H = cap_.nextY + 8;

const svg =
  `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H.toFixed(0)}" viewBox="0 0 ${W} ${H.toFixed(0)}" role="img" aria-label="Throughput by routing strategy: chain bottlenecks, spread and capped use the whole pool">\n` +
  `  <defs>${defs}</defs>\n` +
  `  <rect width="${W}" height="${H.toFixed(0)}" fill="${C.bg}"/>\n` +
  `  <text x="${W / 2}" y="${TITLE_Y}" fill="${C.text}" font-family="${FONT}" font-size="21" font-weight="800" text-anchor="middle">Strict failover wastes the pool. Load-aware routing uses all of it.</text>\n` +
  `  ${pill}\n` +
  `  ${grid}\n` +
  `  ${refs}\n` +
  `  ${bars}\n` +
  `  ${cap_.svg}\n` +
  `</svg>\n`;

writeFileSync(join(repoRoot, "assets", "benchmark.svg"), svg);

console.log("rendered README block + assets/benchmark.svg");
