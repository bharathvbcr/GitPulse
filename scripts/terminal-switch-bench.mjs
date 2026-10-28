#!/usr/bin/env node
/**
 * Runs `harness/terminal-switch.html` in headless Chrome or in the system
 * WKWebView and prints, per arm, how long a hidden WebGL terminal takes to
 * draw again when it is shown: with its context held through the hide
 * ("held", what `gpuRenderer.ts` does for 5 minutes) and with it released and
 * created again ("fresh", the hold at 0 ms). It also prints what one held
 * context occupies.
 *
 *   node scripts/terminal-switch-bench.mjs <chrome|webkit> [trialsPerArm] [deadlineSeconds]
 *
 * The arms alternate A B B A on one terminal. Read the min and the median, and
 * repeat the run: one run is one sample of the host's noise. Run it from the
 * checkout to measure (Vite serves from the working directory). WKWebView
 * draws at the display's ratio (2 on a Retina panel); headless Chrome draws at
 * 1 unless `SWITCH_CHROME_ARGS=--force-device-scale-factor=2` is set.
 */
import path from "node:path";
import { fileURLToPath } from "node:url";
import { runHarnessPage } from "./harness-page-runner.mjs";

const PAGE = "/harness/terminal-switch.html";

/** @param {number[]} values */
const sorted = (values) => [...values].sort((a, b) => a - b);
/** @param {number[]} values @param {number} q */
const quantile = (values, q) => {
  const list = sorted(values);
  return list[Math.min(list.length - 1, Math.floor(q * list.length))];
};
/** @param {number} ms */
const fmt = (ms) => ms.toFixed(1);
/** @param {number} bytes */
const mib = (bytes) => `${(bytes / 1048576).toFixed(1)} MiB`;

/**
 * One line per arm and metric, then the paired comparison and the footprint.
 * A pair is a held trial and the fresh trial next to it in the A B B A order,
 * so the two share the host's state at that moment.
 * @param {any[]} rows
 */
export function summarize(rows) {
  const trials = rows.filter((row) => row.kind === "trial");
  const lines = [];
  for (const metric of ["syncMs", "firstDrawMs", "frameMs"]) {
    for (const arm of ["held", "fresh"]) {
      const values = trials.filter((row) => row.arm === arm).map((row) => row[metric]);
      if (!values.length) continue;
      lines.push(`${metric.padEnd(12)} ${arm.padEnd(6)} n=${values.length} min=${fmt(quantile(values, 0))} med=${fmt(quantile(values, 0.5))} p90=${fmt(quantile(values, 0.9))} max=${fmt(quantile(values, 1))}`);
    }
  }
  const byIndex = sorted(trials.map((row) => row.index)).map((index) => trials.find((row) => row.index === index));
  let freshSlower = 0;
  let pairs = 0;
  /** @type {number[]} */
  const gaps = [];
  for (let at = 0; at + 1 < byIndex.length; at += 2) {
    const [a, b] = [byIndex[at], byIndex[at + 1]];
    if (!a || !b || a.arm === b.arm) continue;
    const held = a.arm === "held" ? a : b;
    const fresh = a.arm === "held" ? b : a;
    pairs += 1;
    gaps.push(fresh.firstDrawMs - held.firstDrawMs);
    if (fresh.firstDrawMs > held.firstDrawMs) freshSlower += 1;
  }
  if (pairs) lines.push(`pairs        fresh slower in ${freshSlower}/${pairs}, firstDraw gap min=${fmt(quantile(gaps, 0))} med=${fmt(quantile(gaps, 0.5))} max=${fmt(quantile(gaps, 1))} ms`);
  for (const row of rows.filter((row) => row.kind === "memory")) {
    lines.push(`footprint    dpr=${row.dpr} ${row.cols}x${row.rows} drawingBuffer=${mib(row.drawingBufferBytes)} atlas=${row.atlasPages} page(s) ${row.atlasPageSizes} = ${mib(row.atlasBytes)} maxTexture=${row.maxTextureSize} maxPages=${row.maxAtlasPages}`);
  }
  return lines;
}

async function main() {
  const [engine = "chrome", trials = "15", seconds = "300"] = process.argv.slice(2);
  const { results, errors, rows } = await runHarnessPage({
    engine: /** @type {"chrome" | "webkit"} */ (engine), page: PAGE, query: `trials=${Number(trials)}`, deadlineSeconds: Number(seconds),
    chromeArgs: (process.env.SWITCH_CHROME_ARGS ?? "").split(" ").filter(Boolean),
  });
  const failed = results.filter((check) => !check.pass);
  console.log(JSON.stringify({ engine, checks: results.length, failed, errors }));
  for (const line of summarize(rows)) console.log(line);
  if (failed.length || errors.length) process.exitCode = 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
