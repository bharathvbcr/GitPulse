#!/usr/bin/env node
/**
 * Compare a benchmark run against the committed baseline for its runner class.
 *
 * The benches under `src-tauri/benches` write one JSON report each (see
 * `benches/common/mod.rs`). This reads a directory of them and fails when
 *
 *   - a **work counter** changed (rows, ops, git processes: deterministic for a
 *     fixed fixture, so any difference is a change in what the code does, not
 *     noise, and it must be reviewed rather than absorbed); or
 *   - a row's **min-of-N** time exceeds the baseline's by more than `--ratio`
 *     (default 1.5) *and* by more than `--floor-us` (default 50 us), so a
 *     sub-microsecond row cannot fail on scheduler jitter; or
 *   - a baselined bench or row is missing from the run (coverage dropped).
 *
 * Times are compared only within one runner class; the baseline records the
 * class it was taken on and a run from another class is refused, not compared.
 * Min-of-N, not p95: on a shared runner the minimum is the figure that load
 * does not inflate. The ratio and floor are documented in docs/PERFORMANCE.md
 * ("Benchmark harness") and are the defaults below.
 *
 * Usage:
 *   node scripts/bench-compare.mjs --results <dir> --baseline <file>
 *        [--ratio 1.5] [--floor-us 50] [--strict-new]
 *   node scripts/bench-compare.mjs --results <dir> --write-baseline <file>
 *
 * Exit codes: 0 no regression · 1 a regression · 2 the comparison could not run
 * (no baseline for this class, a class mismatch, a missing or malformed report).
 * "No baseline" is exit 2, never 0: a check that could not run must not read as
 * one that passed.
 */
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const DEFAULT_RATIO = 1.5;
export const DEFAULT_FLOOR_US = 50;
export const SCHEMA = 1;

/** @param {unknown} error */
const messageOf = (error) => (error instanceof Error ? error.message : String(error));

/**
 * Read every `*.json` bench report in `dir`.
 * @param {string} dir
 * @returns {Record<string, any>} keyed by bench name
 */
export function loadResults(dir) {
  if (!existsSync(dir)) throw new Error(`results directory ${dir} does not exist`);
  const files = readdirSync(dir).filter((name) => name.endsWith(".json")).sort();
  if (files.length === 0) throw new Error(`no bench reports (*.json) in ${dir}`);
  /** @type {Record<string, any>} */
  const results = {};
  for (const name of files) {
    const file = path.join(dir, name);
    let report;
    try {
      report = JSON.parse(readFileSync(file, "utf8"));
    } catch (error) {
      throw new Error(`${file}: not valid JSON (${messageOf(error)})`);
    }
    validateReport(report, file);
    if (results[report.bench]) throw new Error(`${file}: a second report for bench ${report.bench}`);
    results[report.bench] = report;
  }
  return results;
}

/**
 * @param {any} report
 * @param {string} where
 */
export function validateReport(report, where) {
  if (!report || typeof report !== "object") throw new Error(`${where}: not an object`);
  if (report.schema !== SCHEMA) throw new Error(`${where}: schema ${report.schema}, expected ${SCHEMA}`);
  for (const field of ["bench", "runner_class", "os", "arch"]) {
    if (typeof report[field] !== "string" || report[field] === "") {
      throw new Error(`${where}: missing string field ${field}`);
    }
  }
  if (!Number.isInteger(report.cpu_count)) throw new Error(`${where}: missing cpu_count`);
  if (!report.rows || typeof report.rows !== "object" || Object.keys(report.rows).length === 0) {
    throw new Error(`${where}: no rows`);
  }
  for (const [name, row] of Object.entries(report.rows)) {
    if (!Number.isFinite(/** @type {any} */ (row).min_ns) || /** @type {any} */ (row).min_ns < 0) {
      throw new Error(`${where}: row ${name} has no min_ns`);
    }
  }
}

/**
 * Collapse a run into a committed baseline: the min and counters per row, and
 * enough provenance to judge how much to trust it.
 * @param {Record<string, any>} results
 */
export function buildBaseline(results) {
  const reports = Object.values(results);
  const classes = new Set(reports.map((r) => r.runner_class));
  if (classes.size !== 1) throw new Error(`reports span runner classes ${[...classes].join(", ")}`);
  const sample = reports[0];
  /** @type {Record<string, any>} */
  const benches = {};
  for (const report of reports) {
    /** @type {Record<string, any>} */
    const rows = {};
    for (const [name, row] of Object.entries(report.rows)) {
      const r = /** @type {any} */ (row);
      rows[name] = {
        n: r.n,
        time_gated: r.time_gated !== false,
        min_ns: r.min_ns,
        counters: r.counters ?? {},
      };
    }
    benches[report.bench] = {
      commit: report.commit ?? null,
      smoke: Boolean(report.smoke),
      cpu_count: report.cpu_count,
      load1: report.host?.start?.load?.value?.[0] ?? null,
      rows,
    };
  }
  return {
    schema: SCHEMA,
    runner_class: sample.runner_class,
    os: sample.os,
    arch: sample.arch,
    benches,
  };
}

/**
 * @typedef {{ ratio?: number, floorUs?: number, strictNew?: boolean }} Options
 * @typedef {{ code: 0 | 1 | 2, failures: string[], notes: string[] }} Verdict
 */

/**
 * @param {any} baseline
 * @param {Record<string, any>} results
 * @param {Options} [options]
 * @returns {Verdict}
 */
export function compare(baseline, results, options = {}) {
  const ratio = options.ratio ?? DEFAULT_RATIO;
  const floorNs = (options.floorUs ?? DEFAULT_FLOOR_US) * 1000;
  /** @type {string[]} */
  const failures = [];
  /** @type {string[]} */
  const notes = [];
  if (!baseline || baseline.schema !== SCHEMA || typeof baseline.benches !== "object") {
    return { code: 2, failures: ["the baseline is not a schema-1 baseline file"], notes };
  }
  for (const report of Object.values(results)) {
    if (report.runner_class !== baseline.runner_class) {
      return {
        code: 2,
        failures: [
          `runner class mismatch: the baseline is for ${baseline.runner_class}, bench ${report.bench} ran on ${report.runner_class}. ` +
            `Times are only comparable within one class; seed a baseline for ${report.runner_class} instead.`,
        ],
        notes,
      };
    }
  }
  for (const [bench, base] of Object.entries(baseline.benches)) {
    const run = results[bench];
    if (!run) {
      failures.push(`${bench}: baselined but produced no report in this run`);
      continue;
    }
    const load1 = run.host?.start?.load?.value?.[0];
    if (typeof load1 === "number" && run.cpu_count > 0 && load1 / run.cpu_count > 1) {
      notes.push(
        `${bench}: host load ${load1.toFixed(1)} on ${run.cpu_count} cpus; wall-clock rows are inflated, counters are not`,
      );
    }
    for (const [name, baseRow] of Object.entries(/** @type {any} */ (base).rows)) {
      const row = run.rows[name];
      const where = `${bench} / ${name}`;
      if (!row) {
        failures.push(`${where}: baselined row is missing from this run`);
        continue;
      }
      const b = /** @type {any} */ (baseRow);
      const counters = new Set([...Object.keys(b.counters ?? {}), ...Object.keys(row.counters ?? {})]);
      for (const key of [...counters].sort()) {
        const was = b.counters?.[key];
        const now = row.counters?.[key];
        if (was !== now) failures.push(`${where}: work counter ${key} changed ${was ?? "(absent)"} -> ${now ?? "(absent)"}`);
      }
      if (b.time_gated !== false && row.time_gated !== false) {
        const grew = row.min_ns - b.min_ns;
        if (row.min_ns > b.min_ns * ratio && grew > floorNs) {
          failures.push(
            `${where}: min-of-${row.n} ${fmt(row.min_ns)} vs baseline ${fmt(b.min_ns)} ` +
              `(${(row.min_ns / b.min_ns).toFixed(2)}x, limit ${ratio}x and ${fmt(floorNs)} absolute)`,
          );
        }
      }
    }
    for (const name of Object.keys(run.rows)) {
      if (!(/** @type {any} */ (base).rows[name])) {
        const line = `${bench} / ${name}: no baseline for this row`;
        if (options.strictNew) failures.push(line);
        else notes.push(line);
      }
    }
  }
  for (const bench of Object.keys(results)) {
    if (!baseline.benches[bench]) {
      const line = `${bench}: ran but has no baseline`;
      if (options.strictNew) failures.push(line);
      else notes.push(line);
    }
  }
  return { code: failures.length > 0 ? 1 : 0, failures, notes };
}

/** @param {number} ns */
function fmt(ns) {
  if (ns >= 1e9) return `${(ns / 1e9).toFixed(2)}s`;
  if (ns >= 1e6) return `${(ns / 1e6).toFixed(2)}ms`;
  if (ns >= 1e3) return `${(ns / 1e3).toFixed(1)}us`;
  return `${ns}ns`;
}

/** @param {string[]} argv */
export function parseArgs(argv) {
  /** @type {{ results?: string, baseline?: string, writeBaseline?: string, ratio?: number, floorUs?: number, strictNew: boolean }} */
  const out = { strictNew: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const value = () => {
      const next = argv[(i += 1)];
      if (next === undefined) throw new Error(`${arg} needs a value`);
      return next;
    };
    if (arg === "--results") out.results = value();
    else if (arg === "--baseline") out.baseline = value();
    else if (arg === "--write-baseline") out.writeBaseline = value();
    else if (arg === "--ratio") out.ratio = Number(value());
    else if (arg === "--floor-us") out.floorUs = Number(value());
    else if (arg === "--strict-new") out.strictNew = true;
    else throw new Error(`unknown argument ${arg}`);
  }
  if (!out.results) throw new Error("--results <dir> is required");
  if (Boolean(out.baseline) === Boolean(out.writeBaseline)) {
    throw new Error("exactly one of --baseline <file> or --write-baseline <file> is required");
  }
  for (const key of /** @type {const} */ (["ratio", "floorUs"])) {
    if (out[key] !== undefined && !(out[key] > 0)) throw new Error(`--${key === "ratio" ? "ratio" : "floor-us"} must be positive`);
  }
  return out;
}

/** @param {string[]} argv */
export function main(argv) {
  let args;
  try {
    args = parseArgs(argv);
  } catch (error) {
    console.error(`bench-compare: ${messageOf(error)}`);
    return 2;
  }
  let results;
  try {
    results = loadResults(/** @type {string} */ (args.results));
  } catch (error) {
    console.error(`bench-compare: ${messageOf(error)}`);
    return 2;
  }
  if (args.writeBaseline) {
    try {
      const baseline = buildBaseline(results);
      mkdirSync(path.dirname(args.writeBaseline), { recursive: true });
      writeFileSync(args.writeBaseline, `${JSON.stringify(baseline, null, 2)}\n`);
      const rows = Object.values(baseline.benches).reduce((n, b) => n + Object.keys(b.rows).length, 0);
      console.log(`bench-compare: wrote ${args.writeBaseline} (${Object.keys(baseline.benches).length} benches, ${rows} rows, class ${baseline.runner_class})`);
      return 0;
    } catch (error) {
      console.error(`bench-compare: ${messageOf(error)}`);
      return 2;
    }
  }
  const baselinePath = /** @type {string} */ (args.baseline);
  if (!existsSync(baselinePath)) {
    const class_ = Object.values(results)[0]?.runner_class ?? "unknown";
    console.error(
      `bench-compare: NO BASELINE for runner class ${class_} (${baselinePath} does not exist). ` +
        `Nothing was compared. Seed it from this run: node scripts/bench-compare.mjs --results <dir> --write-baseline ${baselinePath}`,
    );
    return 2;
  }
  let baseline;
  try {
    baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
  } catch (error) {
    console.error(`bench-compare: ${baselinePath}: ${messageOf(error)}`);
    return 2;
  }
  const verdict = compare(baseline, results, { ratio: args.ratio, floorUs: args.floorUs, strictNew: args.strictNew });
  for (const note of verdict.notes) console.log(`note: ${note}`);
  for (const failure of verdict.failures) console.error(`FAIL: ${failure}`);
  if (verdict.code === 0) {
    const rows = Object.values(results).reduce((n, r) => n + Object.keys(r.rows).length, 0);
    console.log(`OK: ${Object.keys(results).length} benches, ${rows} rows within ${args.ratio ?? DEFAULT_RATIO}x of baseline ${baseline.runner_class}`);
  }
  return verdict.code;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = main(process.argv.slice(2));
}
