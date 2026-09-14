#!/usr/bin/env node
/**
 * Run every gate CI runs, on this machine, without letting one failure hide
 * the rest.
 *
 * `ci:local` used to be a single `&&` chain. That is the one shape
 * `.github/workflows/ci.yml` deliberately refuses: every step there carries
 * `if: ${{ !cancelled() }}`, with the reason written next to it — otherwise
 * "the first failure hides all of them, so a red build reports one problem per
 * push and each fix reveals the next". The local gate had exactly the defect
 * the remote one was hardened against, so the machine that could have reported
 * four problems at once reported them one push at a time.
 *
 * Two smaller divergences came with it. The WKWebView sweep, which CI runs on
 * its macOS leg, was not in the chain at all — so the engine that produced the
 * only CI-only failure of the last release was the one engine never exercised
 * before pushing. And the Rust step lacked the `--no-fail-fast` and `--locked`
 * that CI passes, so locally the first failing test *binary* ended the run.
 *
 * Every gate runs. Nothing is skipped silently: a gate that cannot run on this
 * platform is reported as SKIPPED with the reason and counted separately,
 * because a check that could not run must never read like one that passed.
 *
 * Exit codes: 0 every gate that ran passed · 1 at least one gate failed.
 */

import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

const CARGO_MANIFEST = "src-tauri/Cargo.toml";

/**
 * The gates, in the order CI runs them.
 *
 * `covers` lists the command lines in `.github/workflows/*.yml` that this gate
 * stands in for. It is not decoration: `ci-local.test.ts` derives the
 * workflows' own commands and fails when one of them is not covered here, so a
 * step added to CI cannot quietly stop being run locally. It is checked in
 * both directions, so a gate cannot claim to cover a step CI no longer has.
 *
 * `local: true` marks a gate CI does not run — those are exempt from the
 * coverage contract but still run here.
 *
 * @typedef {{
 *   id: string, name: string, program: string, args: string[],
 *   covers?: string[], onlyOn?: NodeJS.Platform, local?: boolean, why?: string,
 * }} Gate
 * @type {Gate[]}
 */
export const GATES = [
  {
    id: "vendor-drift",
    name: "Vendored crate drift",
    program: "node",
    args: ["scripts/vendor-crates.mjs", "--check", "--allow-drift"],
    local: true,
    why: "vendored sources are compared against upstream checkouts only a developer has",
  },
  {
    id: "vendor-schema",
    name: "Vendored schema vs installed devmap",
    program: "npm",
    args: ["run", "check:vendor-schema"],
    covers: ["npm run check:vendor-schema -- --allow-missing-cli"],
    why: "release.yml runs this with --allow-missing-cli; a developer machine has the CLI, so it is checked strictly here",
  },
  {
    id: "workflow-lint",
    name: "Workflow lint (actionlint)",
    program: "npm",
    args: ["run", "check:workflows"],
    covers: ["./actionlint -color"],
  },
  {
    id: "ipc-contract",
    name: "IPC command contract",
    program: "npm",
    args: ["run", "check:ipc"],
    covers: ["npm run check:ipc"],
  },
  {
    id: "coverage-types",
    name: "Coverage type contract",
    program: "npm",
    args: ["run", "check:types"],
    covers: ["npm run check:types"],
  },
  {
    id: "release-version",
    name: "Release version gate",
    program: "npm",
    args: ["run", "check:release"],
    covers: ["npm run check:release"],
  },
  {
    id: "svelte-check",
    name: "Frontend Svelte and TypeScript check",
    program: "npm",
    args: ["run", "check"],
    covers: ["npm run check"],
  },
  {
    id: "unit-tests",
    name: "Frontend unit tests with coverage",
    program: "npm",
    args: ["run", "coverage"],
    // `coverage` is `vitest run --coverage`: the same suite CI runs as
    // `npm test`, plus the report coverage.yml gates on.
    covers: ["npm test", "npm run coverage"],
  },
  {
    id: "browser-regressions",
    name: "Browser regressions (headless Chrome)",
    program: "npm",
    args: ["run", "test:browser:all"],
    covers: ["npm run test:browser:all"],
  },
  {
    id: "webkit-regressions",
    name: "Native renderer regressions (WKWebView)",
    program: "npm",
    args: ["run", "test:webkit:all"],
    covers: ["npm run test:webkit:all"],
    onlyOn: "darwin",
    why: "WKWebView is reachable only on macOS; CI runs this on its macos-latest leg",
  },
  {
    id: "build",
    name: "Frontend Vite build",
    program: "npm",
    args: ["run", "build"],
    covers: ["npm run build"],
  },
  {
    id: "cargo-fmt",
    name: "Rust format check",
    program: "cargo",
    args: ["fmt", "--manifest-path", CARGO_MANIFEST, "--all", "--", "--check"],
    covers: [`cargo fmt --manifest-path ${CARGO_MANIFEST} --all -- --check`],
  },
  {
    id: "cargo-clippy",
    name: "Rust lint (clippy)",
    program: "cargo",
    args: ["clippy", "--manifest-path", CARGO_MANIFEST, "--all-targets", "--", "-D", "warnings"],
    covers: [`cargo clippy --manifest-path ${CARGO_MANIFEST} --all-targets -- -D warnings`],
  },
  {
    id: "cargo-tests",
    name: "Rust tests with coverage",
    program: "cargo",
    // --no-fail-fast and --locked mirror ci.yml. Without --no-fail-fast cargo
    // stops at the first failing test BINARY and every later one is skipped,
    // which is the same masking this runner exists to remove, one level down.
    args: [
      "llvm-cov",
      "--manifest-path",
      CARGO_MANIFEST,
      "--workspace",
      "--locked",
      "--no-fail-fast",
      "--lcov",
      "--output-path",
      "lcov.info",
      "--",
      "--test-threads=1",
    ],
    covers: [
      `cargo test --manifest-path ${CARGO_MANIFEST} --locked --no-fail-fast -- --nocapture --test-threads=1`,
    ],
  },
  {
    id: "coverage-floor",
    name: "Coverage floor",
    program: "npm",
    args: ["run", "check:coverage"],
    covers: ["npm run check:coverage"],
  },
];

/** @param {Gate} gate @returns {string} */
export function commandLine(gate) {
  return [gate.program, ...gate.args].join(" ");
}

/**
 * Why this gate cannot run here, or null when it can.
 *
 * @param {Gate} gate
 * @param {NodeJS.Platform} platform
 * @returns {string | null}
 */
export function skipReason(gate, platform) {
  if (gate.onlyOn && gate.onlyOn !== platform) {
    return `needs ${gate.onlyOn}, this host is ${platform}`;
  }
  return null;
}

/** @param {Gate} gate */
function runGate(gate) {
  const started = Date.now();
  const result = spawnSync(gate.program, gate.args, {
    cwd: REPO_ROOT,
    stdio: "inherit",
    encoding: "utf8",
    shell: process.platform === "win32",
  });
  return {
    ok: !result.error && result.status === 0,
    detail: result.error ? result.error.message : `exit ${result.status}`,
    ms: Date.now() - started,
  };
}

/**
 * Run every gate, continuing past failures.
 *
 * @param {Gate[]} gates
 * @param {{ run?: (gate: Gate) => { ok: boolean, detail: string, ms: number },
 *           platform?: NodeJS.Platform, log?: (line: string) => void }} [options]
 */
export function runGates(gates, options = {}) {
  const { run = runGate, platform = process.platform, log = console.log } = options;
  const outcomes = [];
  for (const gate of gates) {
    const skipped = skipReason(gate, platform);
    if (skipped) {
      log(`\n— SKIP ${gate.name} (${skipped})`);
      outcomes.push({ gate, status: "skipped", detail: skipped, ms: 0 });
      continue;
    }
    log(`\n▶ ${gate.name}\n  ${commandLine(gate)}`);
    const { ok, detail, ms } = run(gate);
    outcomes.push({ gate, status: ok ? "passed" : "failed", detail, ms });
    if (!ok) log(`✗ ${gate.name} failed (${detail}) — continuing so the rest still report`);
  }
  return outcomes;
}

/** @param {ReturnType<typeof runGates>} outcomes */
export function summarize(outcomes) {
  const lines = ["", "── ci:local ".padEnd(60, "─"), ""];
  /** @type {Record<string, string>} */
  const mark = { passed: "PASS", failed: "FAIL", skipped: "SKIP" };
  for (const { gate, status, detail, ms } of outcomes) {
    const seconds = status === "skipped" ? "" : `${(ms / 1000).toFixed(1)}s`;
    const note = status === "skipped" ? `  (${detail})` : status === "failed" ? `  (${detail})` : "";
    lines.push(`  ${mark[status]}  ${gate.name.padEnd(42)} ${seconds.padStart(7)}${note}`);
  }
  const failed = outcomes.filter((o) => o.status === "failed");
  const skipped = outcomes.filter((o) => o.status === "skipped");
  const passed = outcomes.filter((o) => o.status === "passed");
  lines.push("");
  lines.push(
    `  ${passed.length} passed · ${failed.length} failed · ${skipped.length} skipped` +
      ` of ${outcomes.length} gates`,
  );
  if (skipped.length) {
    // Named rather than counted: a run with skips is not a clean run, and the
    // reader has to be able to see which coverage they did not get.
    lines.push(`  not run here: ${skipped.map((o) => o.gate.name).join(", ")}`);
  }
  if (failed.length) {
    lines.push("");
    for (const { gate } of failed) lines.push(`  failed: ${commandLine(gate)}`);
  }
  lines.push("");
  return lines.join("\n");
}

function usage() {
  return formatUsage({
    name: "ci-local",
    summary: "Run every gate CI runs, locally, without one failure hiding the rest.",
    flags: [
      { flag: "--list", description: "Print the gates and exit" },
      { flag: "--help, -h", description: "Show this message" },
    ],
    exits: "0 every gate that ran passed · 1 at least one gate failed",
  });
}

/** @param {string[]} argv */
export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  if (argv.includes("--list")) {
    for (const gate of GATES) {
      const skipped = skipReason(gate, process.platform);
      console.log(`${skipped ? "skip" : "run "}  ${gate.name.padEnd(42)} ${commandLine(gate)}`);
    }
    return 0;
  }
  const unknown = argv.find((a) => !["--list", "--help", "-h"].includes(a));
  if (unknown) {
    console.error(`FAIL: unknown option ${JSON.stringify(unknown)}\n`);
    console.error(usage());
    return 2;
  }
  const outcomes = runGates(GATES);
  console.log(summarize(outcomes));
  return outcomes.some((o) => o.status === "failed") ? 1 : 0;
}

if (process.argv[1]?.endsWith("ci-local.mjs")) {
  process.exitCode = main();
}
