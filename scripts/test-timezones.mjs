#!/usr/bin/env node
/**
 * Run local-time frontend tests (due dates, pulse metrics, commit cadence) across
 * non-UTC timezones with DST transitions and a half-hour offset.
 *
 * Hosted CI runners default to UTC, where `new Date(y, m, d, h, min)` never hits
 * a spring-forward gap or 23/25-hour local day boundary. Invoking Vitest per zone
 * in child environments catches timezone and DST regressions cross-platform.
 *
 * Exit codes: 0 all timezone runs passed · 1 a timezone run failed · 2 the
 * check could not run (unknown flag, missing test files, missing vitest, or a
 * runtime whose ICU data ignores TZ).
 */

import { spawnSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

export const TIMEZONES = Object.freeze([
  "America/New_York",
  "Pacific/Auckland",
  "Asia/Kolkata",
]);

/**
 * Known CLDR/ICU canonical aliases where a host OS returns the legacy IANA
 * link name for a configured timezone (macOS ICU resolves Asia/Kolkata to
 * Asia/Calcutta while keeping the exact +05:30 offset).
 *
 * @type {Record<string, readonly string[]>}
 */
export const TIMEZONE_ALIASES = Object.freeze({
  "Asia/Kolkata": Object.freeze(["Asia/Kolkata", "Asia/Calcutta"]),
});

/**
 * Return true when an ICU-resolved timezone matches the requested zone or its
 * known CLDR/IANA canonical alias.
 *
 * @param {string} expectedTz
 * @param {string} actualTz
 * @returns {boolean}
 */
export function timezoneMatches(expectedTz, actualTz) {
  if (actualTz === expectedTz) {
    return true;
  }
  const aliases = TIMEZONE_ALIASES[expectedTz];
  return Array.isArray(aliases) ? aliases.includes(actualTz) : false;
}

export const PATTERNS = Object.freeze([
  "src/lib/workbench/taskDue*.test.ts",
  "src/lib/workbench/taskQuickAdd*.test.ts",
  "src/lib/workbench/taskOrganize*.test.ts",
  "src/lib/pulse/metrics*.test.ts",
  "src/lib/metrics/commitCadence*.test.ts",
  "src/lib/metrics/blameTimeline*.test.ts",
]);

export const VITEST_ENTRY = path.join("node_modules", "vitest", "vitest.mjs");
export const ZONE_TIMEOUT_MS = 120_000;
export const PROBE_TIMEOUT_MS = 15_000;

const USAGE = {
  name: "test-timezones",
  summary: "Run local-time Vitest suites under DST and half-hour offset timezones.",
  flags: [
    { flag: "--help, -h", description: "Print usage and exit 0." },
  ],
  exits: "0 all zones passed · 1 at least one zone failed · 2 could not run",
};

/**
 * Convert a single directory-local wildcard pattern (using `*`) into a RegExp.
 *
 * @param {string} basenamePattern
 * @returns {RegExp}
 */
function globToRegex(basenamePattern) {
  const escaped = basenamePattern.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*");
  return new RegExp(`^${escaped}$`);
}

/**
 * Expand `PATTERNS` to sorted repo-relative POSIX test paths, failing closed if
 * any pattern matches zero files (so a rename cannot silently turn into a pass).
 *
 * @param {string} repoRoot
 * @param {readonly string[]} patterns
 * @returns {string[]}
 */
export function resolvePatternFiles(repoRoot = REPO_ROOT, patterns = PATTERNS) {
  /** @type {string[]} */
  const matched = [];
  const seen = new Set();

  for (const pattern of patterns) {
    const slash = pattern.lastIndexOf("/");
    const relDir = slash === -1 ? "." : pattern.slice(0, slash);
    const namePattern = slash === -1 ? pattern : pattern.slice(slash + 1);
    const absDir = path.join(repoRoot, relDir);
    if (!existsSync(absDir)) {
      throw new Error(`Pattern directory missing for "${pattern}": ${absDir}`);
    }
    const regex = globToRegex(namePattern);
    const hits = readdirSync(absDir)
      .filter((entry) => regex.test(entry))
      .sort()
      .map((entry) => (relDir === "." ? entry : `${relDir}/${entry}`));
    if (hits.length === 0) {
      throw new Error(`Pattern matched no files: ${pattern}`);
    }
    for (const hit of hits) {
      if (!seen.has(hit)) {
        seen.add(hit);
        matched.push(hit);
      }
    }
  }
  return matched;
}

/**
 * Verify that spawning Node with `TZ=<tz>` actually changes the child's ICU
 * resolved timeZone, so a stripped ICU runtime fails closed (exit 2) rather
 * than running every suite in UTC and reporting green.
 *
 * @param {string} tz
 * @param {{
 *   nodePath?: string,
 *   spawn?: typeof spawnSync,
 * }} [options]
 * @returns {{ ok: boolean, detail: string }}
 */
export function verifyZoneSupport(tz, options = {}) {
  const nodePath = options.nodePath ?? process.execPath;
  const spawn = options.spawn ?? spawnSync;
  const probeScript =
    "console.log(Intl.DateTimeFormat().resolvedOptions().timeZone);";

  const result = spawn(nodePath, ["-e", probeScript], {
    env: { ...process.env, TZ: tz },
    encoding: "utf8",
    timeout: PROBE_TIMEOUT_MS,
    killSignal: "SIGKILL",
  });

  if (result.error) {
    return { ok: false, detail: `ICU timezone probe failed for ${tz}: ${result.error.message}` };
  }
  if (result.status !== 0) {
    const stderr = typeof result.stderr === "string" ? result.stderr.trim() : "";
    return {
      ok: false,
      detail: stderr || `ICU timezone probe exited with ${String(result.status)} for ${tz}`,
    };
  }
  const actual = typeof result.stdout === "string" ? result.stdout.trim() : "";
  if (!timezoneMatches(tz, actual)) {
    return {
      ok: false,
      detail: `expected TZ=${tz}, got ${actual || "(empty)"}`,
    };
  }
  return { ok: true, detail: actual };
}

/**
 * Run the local-time Vitest suites across all configured timezones without
 * letting an early zone failure hide later zones.
 *
 * @param {string[]} [argv]
 * @param {{
 *   repoRoot?: string,
 *   nodePath?: string,
 *   spawn?: typeof spawnSync,
 *   verifyZone?: typeof verifyZoneSupport,
 *   log?: (...args: unknown[]) => void,
 *   error?: (...args: unknown[]) => void,
 * }} [deps]
 * @returns {number}
 */
export function main(argv = process.argv.slice(2), deps = {}) {
  const log = deps.log ?? console.log;
  const error = deps.error ?? console.error;

  if (wantsHelp(argv)) {
    log(formatUsage(USAGE));
    return 0;
  }
  const unknown = argv.filter((arg) => arg !== "--");
  if (unknown.length > 0) {
    error(`FAIL: unknown option ${unknown[0]} (run with --help for usage)`);
    return 2;
  }

  const repoRoot = deps.repoRoot ?? REPO_ROOT;
  const nodePath = deps.nodePath ?? process.execPath;
  const spawn = deps.spawn ?? spawnSync;
  const verifyZone = deps.verifyZone ?? ((tz) => verifyZoneSupport(tz, { nodePath, spawn }));

  /** @type {string[]} */
  let files;
  try {
    files = resolvePatternFiles(repoRoot, PATTERNS);
  } catch (err) {
    error(`FAIL: ${err instanceof Error ? err.message : String(err)}`);
    return 2;
  }

  const vitestBin = path.join(repoRoot, VITEST_ENTRY);
  if (!existsSync(vitestBin)) {
    error(`FAIL: vitest entry not found at ${vitestBin}`);
    return 2;
  }

  let anyUnrunnable = false;
  let anyFailed = false;
  /** @type {Array<{ tz: string, status: "passed" | "failed" | "unrunnable", detail: string }>} */
  const outcomes = [];

  for (const tz of TIMEZONES) {
    log(`=== TZ=${tz} (${files.length} test files) ===`);
    const probe = verifyZone(tz);
    if (!probe.ok) {
      anyUnrunnable = true;
      outcomes.push({ tz, status: "unrunnable", detail: probe.detail });
      error(`FAIL [could not run]: ${probe.detail}`);
      continue;
    }

    const result = spawn(nodePath, [vitestBin, "run", ...files], {
      cwd: repoRoot,
      env: { ...process.env, TZ: tz },
      stdio: "inherit",
      timeout: ZONE_TIMEOUT_MS,
      killSignal: "SIGKILL",
    });

    if (result.error) {
      anyUnrunnable = true;
      const detail = `vitest could not run in TZ=${tz}: ${result.error.message}`;
      outcomes.push({ tz, status: "unrunnable", detail });
      error(`FAIL [could not run]: ${detail}`);
    } else if (result.status !== 0) {
      anyFailed = true;
      const detail = `vitest exited ${String(result.status)} in TZ=${tz}`;
      outcomes.push({ tz, status: "failed", detail });
      error(`FAIL: ${detail}`);
    } else {
      outcomes.push({ tz, status: "passed", detail: "passed" });
    }
  }

  if (anyUnrunnable) {
    error(
      `Summary: could not verify all timezones (${outcomes.map((o) => `${o.tz}:${o.status}`).join(", ")})`,
    );
    return 2;
  }
  if (anyFailed) {
    error(
      `Summary: timezone tests failed (${outcomes.map((o) => `${o.tz}:${o.status}`).join(", ")})`,
    );
    return 1;
  }

  log(`OK: passed all ${files.length} local-time test files in ${TIMEZONES.join(", ")}`);
  return 0;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
