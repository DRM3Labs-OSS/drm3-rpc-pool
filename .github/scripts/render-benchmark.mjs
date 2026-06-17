#!/usr/bin/env node
// Render the throughput benchmark into assets/benchmark.svg and the README's
// <!-- BENCHMARK:START --> .. <!-- BENCHMARK:END --> block.
//
// Usage: node render-benchmark.mjs <single.json> <pool.json>
//
// Pure Node stdlib, no dependencies. Inputs are the single-line JSON reports
// emitted by `examples/throughput.rs`.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

const [, , singlePath, poolPath] = process.argv;
if (!singlePath || !poolPath) {
  console.error("usage: render-benchmark.mjs <single.json> <pool.json>");
  process.exit(2);
}

function load(path) {
  const text = readFileSync(path, "utf8").trim();
  // The example prints exactly one JSON line; tolerate trailing log lines.
  const line = text.split("\n").find((l) => l.trim().startsWith("{"));
  return JSON.parse(line);
}

const single = load(singlePath);
const pool = load(poolPath);

const pct = (v) => `${(v * 100).toFixed(1)}%`;
const fmt = (v) => (v == null ? "n/a" : String(v));

// ── Markdown block ─────────────────────────────────────────────────────
const now = new Date().toISOString().replace("T", " ").replace(/\..+/, " UTC");

const table =
  `| Mode | Success rate | Throughput (req/s) | p50 latency | p95 latency |\n` +
  `|------|-------------:|-------------------:|------------:|------------:|\n` +
  `| Without pool (single endpoint) | ${pct(single.success_rate)} | ${fmt(single.throughput_rps)} | ${fmt(single.p50_ms)} ms | ${fmt(single.p95_ms)} ms |\n` +
  `| With pool (failover) | ${pct(pool.success_rate)} | ${fmt(pool.throughput_rps)} | ${fmt(pool.p50_ms)} ms | ${fmt(pool.p95_ms)} ms |\n`;

const block =
  `_Auto-generated ${now} — chain \`${pool.chain}\`, ${pool.requests} requests at concurrency ${pool.concurrency}, FREE public endpoints (no key). Real-network field data, not a lab benchmark; numbers vary with live public-RPC conditions._\n\n` +
  `![Throughput: without vs with the pool](./assets/benchmark.svg)\n\n` +
  table +
  `\n_Run it yourself: \`cargo run --release --example throughput\` (see [Throughput benchmark](#throughput-benchmark))._\n`;

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
  readme.slice(0, startIdx + START.length) +
  "\n" +
  block +
  readme.slice(endIdx);
writeFileSync(readmePath, readme);

// ── SVG bar chart (success rate %) ─────────────────────────────────────
const W = 640;
const H = 320;
const PAD_L = 70;
const PAD_R = 30;
const PAD_T = 70;
const PAD_B = 60;
const chartW = W - PAD_L - PAD_R;
const chartH = H - PAD_T - PAD_B;

const bars = [
  { label: "Without pool", value: single.success_rate, color: "#f85149" },
  { label: "With pool", value: pool.success_rate, color: "#3fb950" },
];

const barGap = 60;
const barW = (chartW - barGap) / 2;
const baseY = PAD_T + chartH;

function bar(b, i) {
  const x = PAD_L + i * (barW + barGap);
  const h = Math.max(2, b.value * chartH);
  const y = baseY - h;
  return (
    `<rect x="${x.toFixed(1)}" y="${y.toFixed(1)}" width="${barW.toFixed(1)}" height="${h.toFixed(1)}" rx="6" fill="${b.color}"/>` +
    `<text x="${(x + barW / 2).toFixed(1)}" y="${(y - 10).toFixed(1)}" fill="#e6edf3" font-family="system-ui,Segoe UI,Helvetica,Arial,sans-serif" font-size="18" font-weight="700" text-anchor="middle">${pct(b.value)}</text>` +
    `<text x="${(x + barW / 2).toFixed(1)}" y="${(baseY + 26).toFixed(1)}" fill="#8b949e" font-family="system-ui,Segoe UI,Helvetica,Arial,sans-serif" font-size="15" text-anchor="middle">${b.label}</text>`
  );
}

// y-axis gridlines at 0/25/50/75/100%.
let grid = "";
for (let p = 0; p <= 100; p += 25) {
  const y = baseY - (p / 100) * chartH;
  grid +=
    `<line x1="${PAD_L}" y1="${y.toFixed(1)}" x2="${(W - PAD_R).toFixed(1)}" y2="${y.toFixed(1)}" stroke="#21262d" stroke-width="1"/>` +
    `<text x="${(PAD_L - 10).toFixed(1)}" y="${(y + 4).toFixed(1)}" fill="#6e7681" font-family="system-ui,Segoe UI,Helvetica,Arial,sans-serif" font-size="11" text-anchor="end">${p}%</text>`;
}

const svg =
  `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" role="img" aria-label="Success rate: without vs with the pool">\n` +
  `  <rect width="${W}" height="${H}" fill="#0d1117"/>\n` +
  `  <text x="${W / 2}" y="34" fill="#e6edf3" font-family="system-ui,Segoe UI,Helvetica,Arial,sans-serif" font-size="20" font-weight="700" text-anchor="middle">Success rate — without vs with the pool</text>\n` +
  `  <text x="${W / 2}" y="54" fill="#8b949e" font-family="system-ui,Segoe UI,Helvetica,Arial,sans-serif" font-size="12" text-anchor="middle">chain ${pool.chain} · ${pool.requests} req · concurrency ${pool.concurrency} · ${now}</text>\n` +
  `  ${grid}\n` +
  `  ${bars.map(bar).join("\n  ")}\n` +
  `</svg>\n`;

writeFileSync(join(repoRoot, "assets", "benchmark.svg"), svg);

console.log("rendered README block + assets/benchmark.svg");
console.log(table);
