#!/usr/bin/env node
// Render the throughput SWEEP into assets/benchmark.svg and the README's
// <!-- BENCHMARK:START --> .. <!-- BENCHMARK:END --> block.
//
// Usage: node render-benchmark.mjs <sweep.jsonl> [more.jsonl ...]
//
// Each input file holds one or more single-line JSON reports emitted by
// `examples/throughput.rs --mode sweep` (or per-pool-size `--mode pool`/single
// runs from separate CI runners). Every record carries a `pool_size` field; we
// sort by it and render a multi-bar chart proving success rate / throughput
// climb as providers are added. Pure Node stdlib, no dependencies.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

const inputs = process.argv.slice(2);
if (inputs.length === 0) {
  console.error("usage: render-benchmark.mjs <sweep.jsonl> [more.jsonl ...]");
  process.exit(2);
}

function loadAll(path) {
  return readFileSync(path, "utf8")
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.startsWith("{"))
    .map((l) => JSON.parse(l));
}

// Collect every record, dedupe by pool_size (last write wins), sort ascending.
const bySize = new Map();
for (const path of inputs) {
  for (const rec of loadAll(path)) {
    if (typeof rec.pool_size !== "number") {
      console.error(`record missing pool_size in ${path}: ${JSON.stringify(rec)}`);
      process.exit(1);
    }
    bySize.set(rec.pool_size, rec);
  }
}
const rows = [...bySize.values()].sort((a, b) => a.pool_size - b.pool_size);
if (rows.length === 0) {
  console.error("no benchmark records found");
  process.exit(1);
}

const meta = rows[0];
const pct = (v) => `${(v * 100).toFixed(1)}%`;
const fmt = (v) => (v == null ? "n/a" : String(v));
const now = new Date().toISOString().replace("T", " ").replace(/\..+/, " UTC");

// ── Markdown block ─────────────────────────────────────────────────────
const header =
  `**${meta.requests} requests · concurrency ${meta.concurrency} · chain \`${meta.chain}\` · FREE public endpoints (no key)**`;

const method =
  `_Method: we fire **${meta.requests} requests** at **concurrency ${meta.concurrency}** (${meta.concurrency} calls in flight at once) against a pool of 1 provider, then 2, then 3, on up. \`Throughput (req/s)\` is **successful** \`eth_blockNumber\` calls per second sustained over the burst (ok-only: failed/rate-limited calls are excluded), not a count of requests. This is single-runner, real-network field data, not a controlled lab benchmark._`;

const table =
  `| Providers | Mode | Success rate | Throughput (req/s) | p50 latency | p95 latency |\n` +
  `|----------:|------|-------------:|-------------------:|------------:|------------:|\n` +
  rows
    .map(
      (r) =>
        `| ${r.pool_size} | ${r.mode === "single" || r.pool_size === 1 ? "single (no failover)" : "pool (failover)"} | ${pct(r.success_rate)} | ${fmt(r.throughput_rps)} | ${fmt(r.p50_ms)} ms | ${fmt(r.p95_ms)} ms |`,
    )
    .join("\n") +
  "\n";

const block =
  `${header}\n\n` +
  `${method}\n\n` +
  `![One provider buckles, a pool holds: success rate and throughput across pool sizes](./assets/benchmark.svg)\n\n` +
  table +
  `\n_Auto-generated ${now}. Real-network field data against free public endpoints, not a lab benchmark; numbers vary with live public-RPC conditions. A single public endpoint gets rate-limited (HTTP 429) under this burst and collapses; with a pool, failover routes around the throttled endpoint so the burst is absorbed and sustained throughput climbs as load spreads across providers. At this load a pool of 2 already absorbs the burst, so the success-rate gain past 2 is small and noisy (public endpoints share one IP on a single runner, and their rate-limit windows overlap run to run); the climb to watch is throughput. Heavier bursts push the success-rate ceiling out to more providers._\n\n` +
  `_Run it yourself: \`cargo run --release --example throughput\` (see [Throughput benchmark](#throughput-benchmark))._\n`;

// ── README rewrite ─────────────────────────────────────────────────────
const readmePath = join(repoRoot, "README.md");
let readme = readFileSync(readmePath, "utf8");
const START = "<!-- BENCHMARK:START -->";
const END = "<!-- BENCHMARK:END -->";
const startIdx = readme.indexOf(START);
const endIdx = readme.indexOf(END);
if (startIdx === -1 || endIdx === -1) {
  console.error("README is missing the BENCHMARK markers");
  process.exit(1);
}
readme =
  readme.slice(0, startIdx + START.length) + "\n" + block + readme.slice(endIdx);
writeFileSync(readmePath, readme);

// ── SVG multi-bar chart ────────────────────────────────────────────────
// Layout is computed top-down with explicit, non-overlapping bands so nothing
// ever collides: title → params pill → chart (gridlines + bars + value labels
// above bars) → x-axis labels → footnote.
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
  good: "#3fb950",
  goodTop: "#56d364",
  bad: "#f85149",
  badTop: "#ff7b72",
  warn: "#d29922",
};

const n = rows.length;
const W = Math.max(720, 150 + n * 110);

// Vertical bands (y positions), each with clear breathing room. The bottom
// text bands (caption + footnote) are laid out dynamically AFTER wrapping so
// nothing ever clips the SVG edges; H is computed last from the real bottom.
const TITLE_Y = 40;
const PILL_TOP = 60;
const PILL_H = 40;
const CHART_TOP = PILL_TOP + PILL_H + 36; // top of plot area
const CHART_H = 230; // plot area height
const CHART_BOTTOM = CHART_TOP + CHART_H; // baseline (0%)
const XLABEL_Y = CHART_BOTTOM + 28; // "N provider(s)" row
const XSUB_Y = XLABEL_Y + 18; // throughput sub-label row
const CAPTION_Y = XSUB_Y + 30; // first methodology-caption baseline

const PAD_L = 62;
const PAD_R = 28;
const plotW = W - PAD_L - PAD_R;

// Bars: leave ~18% gap on each side of a slot, bar fills the middle.
const slotW = plotW / n;
const barW = Math.min(76, slotW * 0.56);

function yFor(rate) {
  return CHART_BOTTOM - rate * CHART_H;
}

// Gridlines + y-axis labels at 0/25/50/75/100%.
let grid = "";
for (let p = 0; p <= 100; p += 25) {
  const y = CHART_BOTTOM - (p / 100) * CHART_H;
  grid +=
    `<line x1="${PAD_L}" y1="${y.toFixed(1)}" x2="${(W - PAD_R).toFixed(1)}" y2="${y.toFixed(1)}" stroke="${C.grid}" stroke-width="1"/>` +
    `<text x="${(PAD_L - 12).toFixed(1)}" y="${(y + 4).toFixed(1)}" fill="${C.axis}" font-family="${FONT}" font-size="12" text-anchor="end">${p}%</text>`;
}

// Bars (success rate) with value labels safely ABOVE each bar.
const maxRps = Math.max(...rows.map((r) => r.throughput_rps || 0), 1);
let bars = "";
let defs = "";
rows.forEach((r, i) => {
  const cx = PAD_L + slotW * i + slotW / 2;
  const x = cx - barW / 2;
  const rate = r.success_rate;
  const top = yFor(rate);
  const h = Math.max(3, CHART_BOTTOM - top);
  const single = r.mode === "single" || r.pool_size === 1;
  const gid = `g${i}`;
  const c0 = single ? C.bad : C.good;
  const c1 = single ? C.badTop : C.goodTop;
  defs +=
    `<linearGradient id="${gid}" x1="0" y1="0" x2="0" y2="1">` +
    `<stop offset="0" stop-color="${c1}"/><stop offset="1" stop-color="${c0}"/></linearGradient>`;

  // Value label (success %) above the bar, guaranteed clear of the bar top.
  const labelY = top - 12;
  bars +=
    `<rect x="${x.toFixed(1)}" y="${top.toFixed(1)}" width="${barW.toFixed(1)}" height="${h.toFixed(1)}" rx="7" fill="url(#${gid})"/>` +
    `<text x="${cx.toFixed(1)}" y="${labelY.toFixed(1)}" fill="${C.text}" font-family="${FONT}" font-size="18" font-weight="800" text-anchor="middle">${pct(rate)}</text>`;
});

// X-axis labels: pool size (bold) + throughput sub-label.
let xlabels = "";
rows.forEach((r, i) => {
  const cx = PAD_L + slotW * i + slotW / 2;
  const label = `${r.pool_size} provider${r.pool_size === 1 ? "" : "s"}`;
  xlabels +=
    `<text x="${cx.toFixed(1)}" y="${XLABEL_Y}" fill="${C.text}" font-family="${FONT}" font-size="14" font-weight="700" text-anchor="middle">${label}</text>` +
    `<text x="${cx.toFixed(1)}" y="${XSUB_Y}" fill="${C.sub}" font-family="${FONT}" font-size="12" text-anchor="middle">${fmt(r.throughput_rps)} req/s</text>`;
});

// Prominent run-parameter pill (the thing that must JUMP OUT).
const pillSegs = [
  `${meta.requests} requests`,
  `concurrency ${meta.concurrency}`,
  `chain ${meta.chain}`,
  `free public RPCs`,
];
let pill = "";
{
  const padX = 18;
  const gap = 14;
  const dotR = 3;
  const fontSize = 15;
  const charW = fontSize * 0.62;
  // Approximate segment widths to center the pill.
  const segW = pillSegs.map((s) => s.length * charW);
  const innerW =
    segW.reduce((a, b) => a + b, 0) + (pillSegs.length - 1) * (gap + dotR * 2 + gap);
  const pillW = innerW + padX * 2;
  const px = (W - pillW) / 2;
  pill +=
    `<rect x="${px.toFixed(1)}" y="${PILL_TOP}" width="${pillW.toFixed(1)}" height="${PILL_H}" rx="${PILL_H / 2}" fill="${C.panel}" stroke="${C.border}" stroke-width="1"/>`;
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

// Wrap `text` to lines that fit inside the viewport (greedy by word), centered
// at `cx`, starting at baseline `y0`. Returns the SVG and the next free y so
// the next text band can stack right below without ever overlapping or
// clipping. `pad` is the horizontal margin reserved on EACH side.
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

// Methodology caption: terse, honest statement of what was measured.
const caption =
  `We fire ${meta.requests} requests at concurrency ${meta.concurrency} (${meta.concurrency} in flight at once). req/s is successful calls per second sustained over the burst, not a request count. Single-runner, real-network field data.`;

// Footnote: legend + honest "2 suffice at this load" note, plus the timestamp.
const footnote =
  `Bars = success rate, sub-labels = sustained req/s, per provider count. At this load a pool of 2 already absorbs the burst; heavier bursts need more providers. Auto-generated ${now}. Real-network, varies with live public-RPC conditions.`;

const cap = wrapCentered(caption, W / 2, CAPTION_Y, {
  size: 12.5,
  lineH: 17,
  fill: C.sub,
  weight: "600",
});
const foot = wrapCentered(footnote, W / 2, cap.nextY + 16, {
  size: 11,
  lineH: 15,
  fill: C.axis,
});
const H = foot.nextY + 6;

const svg =
  `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H.toFixed(0)}" viewBox="0 0 ${W} ${H.toFixed(0)}" role="img" aria-label="Success rate and throughput climb as providers are added to the pool">\n` +
  `  <defs>${defs}</defs>\n` +
  `  <rect width="${W}" height="${H.toFixed(0)}" fill="${C.bg}"/>\n` +
  `  <text x="${W / 2}" y="${TITLE_Y}" fill="${C.text}" font-family="${FONT}" font-size="22" font-weight="800" text-anchor="middle">One provider buckles, a pool holds</text>\n` +
  `  ${pill}\n` +
  `  ${grid}\n` +
  `  ${bars}\n` +
  `  ${xlabels}\n` +
  `  ${cap.svg}\n` +
  `  ${foot.svg}\n` +
  `</svg>\n`;

writeFileSync(join(repoRoot, "assets", "benchmark.svg"), svg);

console.log("rendered README block + assets/benchmark.svg");
console.log(table);
