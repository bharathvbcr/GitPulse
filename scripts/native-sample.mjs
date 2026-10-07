#!/usr/bin/env node
/**
 * Samples the installed GitPulse app's resident memory and CPU over time, for
 * the idle and soak rows of docs/QUALIFICATION.md.
 *
 *   node scripts/native-sample.mjs --duration 8h --interval 60 --out soak.jsonl
 *
 * Each line of `--out` is one sample; the summary on stdout is computed from
 * them. Three groups are kept apart, because adding them up would be wrong:
 *
 * - `app`: the GitPulse process itself.
 * - `helpers`: its descendants that run from inside the app bundle.
 * - `hosted`: everything else it started — shells and agent CLIs in its
 *   terminal tabs. Their memory is the user's work, not GitPulse's.
 *
 * Not measured: the WKWebView content and networking processes. macOS starts
 * them through launchd, so they are not descendants and `ps` cannot attribute
 * them to this app. The summary says so rather than reporting a total that
 * looks complete.
 */
import { execFileSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const APP_SUFFIX = "/GitPulse.app/Contents/MacOS/gitpulse";

/** @typedef {{ pid: number, ppid: number, rssKib: number, cpu: number, command: string }} Row */

/** @param {string} text `ps -A -o pid=,ppid=,rss=,%cpu=,comm=` output. @returns {Row[]} */
export function parsePs(text) {
  /** @type {Row[]} */
  const rows = [];
  for (const line of text.split("\n")) {
    const match = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+([\d.]+)\s+(.+?)\s*$/.exec(line);
    if (match) {
      rows.push({ pid: Number(match[1]), ppid: Number(match[2]), rssKib: Number(match[3]), cpu: Number(match[4]), command: match[5] });
    }
  }
  return rows;
}

/**
 * One sample of `root` and its descendants, grouped.
 * @param {Row[]} rows
 * @param {number} root
 */
export function sampleTree(rows, root) {
  const app = rows.find((row) => row.pid === root);
  if (!app) return null;
  const bundle = app.command.slice(0, app.command.length - "/MacOS/gitpulse".length);
  /** @type {Map<number, Row[]>} */
  const children = new Map();
  for (const row of rows) {
    if (!children.has(row.ppid)) children.set(row.ppid, []);
    children.get(row.ppid)?.push(row);
  }
  const group = () => ({ processes: 0, rssKib: 0, cpu: 0 });
  const helpers = group();
  const hosted = group();
  const pending = [...(children.get(root) ?? [])];
  while (pending.length) {
    const row = pending.pop();
    if (!row) break;
    const into = row.command.startsWith(bundle) ? helpers : hosted;
    into.processes += 1;
    into.rssKib += row.rssKib;
    into.cpu += row.cpu;
    pending.push(...(children.get(row.pid) ?? []));
  }
  return { app: { rssKib: app.rssKib, cpu: app.cpu }, helpers, hosted };
}

/**
 * @param {{ t: number, app: { rssKib: number, cpu: number } }[]} samples
 */
export function summarize(samples) {
  if (samples.length === 0) return { samples: 0 };
  const rss = samples.map((s) => s.app.rssKib);
  const hours = samples.map((s) => (s.t - samples[0].t) / 3_600_000);
  // Least-squares slope: a soak's question is the trend, not two endpoints.
  const meanX = hours.reduce((a, b) => a + b, 0) / hours.length;
  const meanY = rss.reduce((a, b) => a + b, 0) / rss.length;
  let num = 0;
  let den = 0;
  for (let i = 0; i < rss.length; i++) {
    num += (hours[i] - meanX) * (rss[i] - meanY);
    den += (hours[i] - meanX) ** 2;
  }
  return {
    samples: samples.length,
    hours: hours[hours.length - 1],
    appRssMib: { first: rss[0] / 1024, last: rss[rss.length - 1] / 1024, min: Math.min(...rss) / 1024, max: Math.max(...rss) / 1024 },
    appRssSlopeMibPerHour: den === 0 ? null : num / den / 1024,
    appCpuMean: samples.reduce((a, s) => a + s.app.cpu, 0) / samples.length,
    notMeasured: "WKWebView content/networking processes (launchd children, not attributable by ps)",
  };
}

/** @param {string | null | undefined} text */
function parseDuration(text) {
  const match = /^(\d+(?:\.\d+)?)(s|m|h)$/.exec(text ?? "");
  if (!match) throw new Error(`--duration must look like 90s, 30m or 8h, not ${text}`);
  const unit = /** @type {"s" | "m" | "h"} */ (match[2]);
  return Number(match[1]) * { s: 1_000, m: 60_000, h: 3_600_000 }[unit];
}

/** @param {Row[]} rows */
function findApp(rows) {
  const apps = rows.filter((row) => row.command.endsWith(APP_SUFFIX));
  if (apps.length !== 1) throw new Error(`expected one running ${APP_SUFFIX}, found ${apps.length}`);
  return apps[0].pid;
}

/** @param {string[]} argv */
async function main(argv) {
  /** @param {string} name @param {string | null} fallback */
  const arg = (name, fallback) => {
    const i = argv.indexOf(name);
    return i === -1 ? fallback : argv[i + 1];
  };
  const durationMs = parseDuration(arg("--duration", "10m"));
  const intervalMs = Number(arg("--interval", "60")) * 1_000;
  const out = arg("--out", null);
  if (!(intervalMs >= 1_000)) throw new Error("--interval must be at least 1 second");
  const ps = () => parsePs(execFileSync("ps", ["-A", "-o", "pid=,ppid=,rss=,%cpu=,comm="], { encoding: "utf8" }));
  const root = Number(arg("--pid", "")) || findApp(ps());
  /** @type {{ t: number, app: { rssKib: number, cpu: number } }[]} */
  const samples = [];
  const end = Date.now() + durationMs;
  for (;;) {
    const tree = sampleTree(ps(), root);
    if (!tree) {
      // The app exiting mid-soak is a result, not a gap to paper over.
      console.error(`process ${root} exited after ${samples.length} sample(s)`);
      break;
    }
    const sample = { t: Date.now(), ...tree };
    samples.push(sample);
    if (out) appendFileSync(out, `${JSON.stringify(sample)}\n`);
    if (Date.now() + intervalMs > end) break;
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
  console.log(JSON.stringify(summarize(samples), null, 2));
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(error instanceof Error ? error.message : error);
    process.exit(1);
  });
}
