#!/usr/bin/env node
/**
 * Attribute disk writes to single Rust tests, and fail any that exceeds a budget.
 *
 * Why one process per test: the operating system counts bytes written per
 * *process*. The test harness runs every test of a binary inside one process,
 * so a process-wide counter cannot say which test wrote what. Running each test
 * alone, `--exact`, makes the process total the test's own bytes. The counter is
 * read by the test binary itself at exit (`src-tauri/src/test_support/env.rs`,
 * `diskio`): `ri_diskio_byteswritten` from `proc_pid_rusage` on macOS,
 * `write_bytes` from `/proc/self/io` on Linux. This script only sets
 * `GITPULSE_TEST_DISKIO_REPORT` and reads the lines back.
 *
 * What it cannot see: a test whose writes depend on state an earlier test left
 * behind. The in-process watchdog in the same module covers that (it aborts a
 * run that averages more than 40 MiB/s over 30 s, or writes more than 8 GiB).
 *
 * Usage: node scripts/test-disk-writes.mjs [--budget-mib N] [--jobs N] [--top N] [--filter TEXT]
 *   --budget-mib  per-test budget; a test writing more fails the run (default 256)
 *   --jobs        tests run at once (default 4); a test's own bytes do not depend on it
 *   --top         how many of the largest writers to list (default 15)
 *   --filter      only tests whose name contains TEXT
 *
 * Exit codes: 0 every measured test is within budget · 1 a test is over budget
 * or a test could not be measured · 2 the harness could not be built or listed.
 */
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const MIB = 1024 * 1024;
export const DEFAULT_BUDGET_MIB = 256;

/**
 * Parses report lines (`<bytes>\t<argv>`) into `{ test, bytes }`, keeping the
 * largest figure when a test appears more than once. Lines that do not parse are
 * returned separately: a line we could not read is a test we could not measure,
 * which must never be counted as a test that wrote nothing.
 *
 * @param {string} text
 */
export function parseReport(text) {
  /** @type {Map<string, number>} */
  const bytesByTest = new Map();
  /** @type {string[]} */
  const unreadable = [];
  for (const line of text.split("\n")) {
    if (line.trim() === "") continue;
    const [bytes, argv = ""] = line.split("\t");
    const match = /^(?:--exact\s+)?(\S+)/.exec(argv.replace(/^.*?--exact\s+/, "--exact "));
    if (!/^\d+$/.test(bytes ?? "") || !match?.[1]) {
      unreadable.push(line);
      continue;
    }
    const value = Number(bytes);
    bytesByTest.set(match[1], Math.max(bytesByTest.get(match[1]) ?? 0, value));
  }
  return { bytesByTest, unreadable };
}

/**
 * The tests over `budgetBytes`, largest first.
 *
 * @param {Map<string, number>} bytesByTest @param {number} budgetBytes
 */
export function overBudget(bytesByTest, budgetBytes) {
  return [...bytesByTest]
    .filter(([, bytes]) => bytes > budgetBytes)
    .sort((a, b) => b[1] - a[1]);
}

/** @param {string[]} argv */
function parseArgs(argv) {
  const options = { budgetMib: DEFAULT_BUDGET_MIB, jobs: 4, top: 15, filter: "" };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    const value = argv[i + 1];
    if (flag === "--budget-mib") options.budgetMib = Number(value);
    else if (flag === "--jobs") options.jobs = Number(value);
    else if (flag === "--top") options.top = Number(value);
    else if (flag === "--filter") options.filter = String(value ?? "");
    else throw new Error(`unknown argument ${flag}`);
    i += 1;
  }
  for (const [name, number] of [["--budget-mib", options.budgetMib], ["--jobs", options.jobs], ["--top", options.top]]) {
    if (!Number.isInteger(number) || number < 1) throw new Error(`${name} needs a positive integer`);
  }
  return options;
}

function libTestBinary() {
  const build = spawnSync(
    "cargo",
    ["test", "--manifest-path", "src-tauri/Cargo.toml", "--lib", "--no-run", "--message-format=json"],
    { cwd: REPO_ROOT, encoding: "utf8", maxBuffer: 256 * MIB },
  );
  if (build.status !== 0) throw new Error(`cargo could not build the lib tests:\n${build.stderr}`);
  for (const line of build.stdout.split("\n")) {
    if (!line.startsWith("{")) continue;
    const message = JSON.parse(line);
    if (message.reason === "compiler-artifact" && message.profile?.test && message.target?.name === "gitpulse_lib" && message.executable) {
      return message.executable;
    }
  }
  throw new Error("cargo reported no lib test executable");
}

function listTests(binary, filter) {
  const listed = spawnSync(binary, ["--list", "--format", "terse"], { encoding: "utf8", maxBuffer: 64 * MIB });
  if (listed.status !== 0) throw new Error(`could not list tests:\n${listed.stderr}`);
  return listed.stdout
    .split("\n")
    .filter((line) => line.endsWith(": test"))
    .map((line) => line.slice(0, -": test".length))
    .filter((name) => name.includes(filter));
}

function runOne(binary, test, reportFile) {
  return new Promise((resolve) => {
    const child = spawn(binary, ["--exact", test, "--test-threads=1"], {
      env: { ...process.env, GITPULSE_TEST_DISKIO_REPORT: reportFile, GITPULSE_TEST_WRITE_BUDGET: "0" },
      stdio: "ignore",
    });
    child.on("close", (code, signal) => resolve({ test, code, signal }));
  });
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const binary = libTestBinary();
  const tests = listTests(binary, options.filter);
  if (tests.length === 0) throw new Error("no tests matched");
  const scratch = mkdtempSync(path.join(os.tmpdir(), "gitpulse-diskio-"));
  const reportFile = path.join(scratch, "report.tsv");
  const queue = [...tests];
  const failed = [];
  await Promise.all(
    Array.from({ length: options.jobs }, async () => {
      for (let test = queue.pop(); test !== undefined; test = queue.pop()) {
        const outcome = await runOne(binary, test, reportFile);
        if (outcome.code !== 0) failed.push(outcome);
      }
    }),
  );
  let report = "";
  try {
    report = readFileSync(reportFile, "utf8");
  } catch {
    /* no test wrote a report: handled below as unmeasured */
  }
  rmSync(scratch, { recursive: true, force: true });
  const { bytesByTest, unreadable } = parseReport(report);
  const unmeasured = tests.filter((test) => !bytesByTest.has(test));
  const sorted = [...bytesByTest].sort((a, b) => b[1] - a[1]);
  const total = sorted.reduce((sum, [, bytes]) => sum + bytes, 0);
  console.log(`measured ${bytesByTest.size} of ${tests.length} tests; ${(total / MIB).toFixed(0)} MiB written in all`);
  console.log(`largest ${Math.min(options.top, sorted.length)} writers:`);
  for (const [test, bytes] of sorted.slice(0, options.top)) {
    console.log(`  ${(bytes / MIB).toFixed(1).padStart(10)} MiB  ${test}`);
  }
  const offenders = overBudget(bytesByTest, options.budgetMib * MIB);
  let ok = true;
  if (offenders.length > 0) {
    ok = false;
    console.error(`\nOVER BUDGET (${options.budgetMib} MiB):`);
    for (const [test, bytes] of offenders) console.error(`  ${(bytes / MIB).toFixed(1)} MiB  ${test}`);
  }
  if (unmeasured.length > 0 || unreadable.length > 0) {
    ok = false;
    console.error(`\n${unmeasured.length} test(s) produced no measurement and ${unreadable.length} report line(s) were unreadable; an unmeasured test is not a test within budget.`);
    for (const test of unmeasured.slice(0, 20)) console.error(`  unmeasured: ${test}`);
  }
  if (failed.length > 0) console.error(`\n${failed.length} test(s) failed when run alone (not a budget finding): ${failed.slice(0, 10).map((f) => f.test).join(", ")}`);
  process.exit(ok ? 0 : 1);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(2);
  });
}
