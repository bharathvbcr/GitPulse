#!/usr/bin/env node
/**
 * Runs `harness/terminal-scroll.html` in headless Chrome or in the system
 * WKWebView and prints, per arm, how a terminal scrolls through a 5,000-row
 * scrollback under scripted wheel input: the time between animation frames
 * (p50, p95, max, and the frames over 16.7 ms and over 33 ms) and how far the
 * viewport moves per frame (the largest step, the median step of a frame that
 * moved, and the frames that did not move between the first move and the
 * last: the stalls).
 *
 *   node scripts/terminal-scroll-bench.mjs <chrome|webkit> [rounds] [deadlineSeconds] [extra query]
 *
 * Arms are renderer (webgl, dom) x input (wheel, pixel, pixel-int) x output
 * (idle, streaming) x xterm's smoothScrollDuration (0 = its default, then
 * 150). Narrow or widen them with the query, e.g.
 * `"renderers=dom&inputs=wheel&smooths=0,100,150,250&sens=1,2"`. Every round
 * runs every arm once, in an order that rotates per round. Each metric is
 * computed per trial, then reported as the min and the median across rounds;
 * the min is the figure least touched by the host's noise, the median says how
 * usual it is. Run it from the checkout to measure (Vite serves from the
 * working directory). The frame cadence is the display's: a 60 Hz panel's
 * frames are 16.7 ms, a 120 Hz panel's 8.3 ms, and the line `cadence` says
 * which this run had.
 */
import path from "node:path";
import { fileURLToPath } from "node:url";
import { runHarnessPage } from "./harness-page-runner.mjs";

/** A frame over one 60 Hz vsync: 16.7 ms plus the 1.8 ms WebKit's whole-millisecond rAF timestamps and vsync jitter add. */
const OVER_VSYNC_MS = 18.5;
const PAGE = "/harness/terminal-scroll.html";

/** @param {number[]} values */
const sorted = (values) => [...values].sort((a, b) => a - b);
/** @param {number[]} values @param {number} q */
export const quantile = (values, q) => {
  if (!values.length) return Number.NaN;
  const list = sorted(values);
  return list[Math.min(list.length - 1, Math.floor(q * list.length))];
};
/** @param {number} value @param {number} [digits] */
const fmt = (value, digits = 1) => (Number.isFinite(value) ? value.toFixed(digits) : "-");

/**
 * What one trial says about its own frames and steps.
 * @param {{ frameMs: number[], dispatchMs?: number[], steps: number[], scrollEvents: number, handlerMs: number }} trial
 */
export function trialMetrics(trial) {
  const { frameMs, steps } = trial;
  const moved = steps.map((step, index) => ({ step, index })).filter(({ step }) => step > 0);
  const first = moved.length ? moved[0].index : 0;
  const last = moved.length ? moved[moved.length - 1].index : -1;
  return {
    p50: quantile(frameMs, 0.5),
    p95: quantile(frameMs, 0.95),
    max: quantile(frameMs, 1),
    over16: frameMs.filter((ms) => ms > OVER_VSYNC_MS).length,
    over33: frameMs.filter((ms) => ms > 33).length,
    frames: frameMs.length,
    maxStep: moved.length ? Math.max(...moved.map(({ step }) => step)) : 0,
    medStep: quantile(moved.map(({ step }) => step), 0.5),
    stalls: last >= first ? steps.slice(first, last + 1).filter((step) => step === 0).length : 0,
    span: last >= first ? last - first + 1 : 0,
    scrollEvents: trial.scrollEvents,
    handlerMs: trial.handlerMs,
    dispatchMax: trial.dispatchMs?.length ? Math.max(...trial.dispatchMs) : 0,
    dispatchP95: quantile(trial.dispatchMs ?? [], 0.95),
  };
}

/** @param {any} row */
export const armKey = (row) => `${row.renderer}/${row.input}/${row.stream ? "stream" : "idle"}/smooth=${row.smooth}/sens=${row.sens}${row.guard ? "/guard" : ""}`;

/** A streaming trial that ended with the view at the bottom: the gesture was lost to the output. */
export const yanked = (/** @type {any} */ trial) => Boolean(trial.stream) && trial.finalY >= trial.endBaseY;

const COLUMNS = ["p50", "p95", "max", "over16", "over33", "maxStep", "stalls", "dispMax"];

/**
 * One line per arm: each metric as `min/median` over the rounds.
 * @param {any[]} rows
 */
export function summarize(rows) {
  const trials = rows.filter((row) => row.kind === "trial");
  /** @type {Map<string, any[]>} */
  const groups = new Map();
  for (const trial of trials) {
    const key = armKey(trial);
    groups.set(key, [...(groups.get(key) ?? []), trial]);
  }
  const lines = [];
  const idle = trials.filter((row) => !row.stream && row.smooth === 0).flatMap((row) => row.frameMs);
  if (idle.length) lines.push(`cadence      median frame ${fmt(quantile(idle, 0.5))} ms over ${idle.length} frames (idle, unsmoothed arms)`);
  lines.push(`${"arm".padEnd(50)} n   ${COLUMNS.map((name) => name.padStart(11)).join(" ")}  (each min/median over n rounds; frame ms, steps in rows)`);
  for (const [key, group] of groups) {
    const metrics = group.map(trialMetrics);
    const cells = COLUMNS.map((name) => {
      const values = metrics.map((metric) => /** @type {any} */ (metric)[name === "dispMax" ? "dispatchMax" : name]);
      const digits = name.startsWith("over") || name === "maxStep" || name === "stalls" ? 0 : 1;
      return `${fmt(quantile(values, 0), digits)}/${fmt(quantile(values, 0.5), digits)}`.padStart(11);
    });
    lines.push(`${key.padEnd(50)} ${String(group.length).padEnd(3)} ${cells.join(" ")}  yanked=${group.filter(yanked).length}/${group.length}`);
  }
  return lines;
}

async function main() {
  const [engine = "chrome", rounds = "5", seconds = "900", extra = ""] = process.argv.slice(2);
  const { results, errors, rows } = await runHarnessPage({
    engine: /** @type {"chrome" | "webkit"} */ (engine), page: PAGE, query: `rounds=${Number(rounds)}${extra ? `&${extra}` : ""}`, deadlineSeconds: Number(seconds),
    chromeArgs: (process.env.SCROLL_CHROME_ARGS ?? "").split(" ").filter(Boolean),
  });
  const failed = results.filter((check) => !check.pass);
  console.log(JSON.stringify({ engine, checks: results.length, failed, errors }));
  for (const line of summarize(rows)) console.log(line);
  if (process.env.SCROLL_BENCH_JSON) {
    const { writeFileSync } = await import("node:fs");
    writeFileSync(process.env.SCROLL_BENCH_JSON, JSON.stringify(rows));
  }
  if (failed.length || errors.length) process.exitCode = 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
}
