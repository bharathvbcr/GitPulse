#!/usr/bin/env node
/**
 * Run the local gates (`--list` prints them) on this machine, without letting
 * one failure hide the rest. It is not all of CI: `bun audit`, `cargo deny`, the
 * Go jobs and the scheduled workflows run only there.
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
 * Many agent sessions edit this working tree while the gates run, so a pass can
 * describe neither the tree the run started on nor the one it ended on. The run
 * therefore reads Git's tree id for the working tree (tracked changes and
 * untracked files in, ignored files out) once before the first gate and once
 * after the last, and the summary says which tree was tested, or that the tree
 * moved, or that it could not be read. `workingTreeId` does this through a
 * private temporary index, so the real index, HEAD and every file are untouched;
 * the only trace is unreferenced blob and tree objects in the object database,
 * which `git gc` collects.
 *
 * `Tested-tree: <id>` is printed as the very last line when, and only when, all
 * of these hold: every gate in GATES ran (none skipped), every one passed, and
 * both tree readings succeeded and are equal. It is evidence of exactly which
 * files passed; any other run prints no such line. (There is no gate filter or
 * subset option; a subset of GATES passed to `execute` never prints it.)
 *
 * Exit codes: 0 every gate that ran passed · 1 at least one gate failed · 2
 * unknown option · 3 only with --strict-tree: every gate that ran passed but the
 * working tree changed during the run or could not be read. Without
 * --strict-tree a changed or unreadable tree is reported and the code is 0/1.
 */

import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdtempSync, rmSync, statSync, utimesSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

const CARGO_MANIFEST = "src-tauri/Cargo.toml";
// Its own workspace, Cargo.lock and target dir: not a member of src-tauri.
const WORKTREE_MANIFEST = "crates/gitpulse-worktree/Cargo.toml";

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
 * `produces` lists the reports a gate writes (repository-relative), which a
 * later gate reads. Each is removed before its gate runs. `cargo llvm-cov`
 * writes no report when a test fails, so without that the coverage floor read
 * the last run's `lcov.info` — eight days old on 2026-10-25 — and passed on it:
 * a check that could not run, reading like one that ran.
 *
 * @typedef {{
 *   id: string, name: string, program: string, args: string[],
 *   covers?: string[], onlyOn?: NodeJS.Platform, local?: boolean, why?: string,
 *   produces?: string[],
 * }} Gate
 * @type {Gate[]}
 */
export const GATES = [
  {
    id: "vendor-edits",
    name: "Vendored crates unedited",
    program: "bun",
    args: ["run", "check:vendor-edits"],
    why: "upstream drift is reported, not failed: it is a refresh signal, and failing on it would make every upstream commit break this gate",
  },
  {
    id: "vendor-schema",
    name: "Vendored schema vs installed devmap",
    program: "bun",
    args: ["run", "check:vendor-schema"],
    covers: ["bun run check:vendor-schema -- --allow-missing-cli"],
    why: "release.yml runs this with --allow-missing-cli; a developer machine has the CLI, so it is checked strictly here",
  },
  {
    id: "workflow-lint",
    name: "Workflow lint (actionlint)",
    program: "bun",
    args: ["run", "check:workflows"],
    covers: ["./actionlint -color"],
  },
  {
    // ci.yml's fixture-authors job checks the pushed range; here it checks what
    // this checkout has not pushed, which is what the next push would add.
    id: "fixture-authors",
    name: "No commit authored as a test fixture",
    program: "bun",
    args: ["run", "check:fixture-authors"],
    covers: ['bun run check:fixture-authors "$TIP" "$BASE"'],
  },
  {
    id: "ipc-contract",
    name: "IPC command contract",
    program: "bun",
    args: ["run", "check:ipc"],
    covers: ["bun run check:ipc"],
  },
  {
    id: "coverage-types",
    name: "Coverage type contract",
    program: "bun",
    args: ["run", "check:types"],
    covers: ["bun run check:types"],
  },
  {
    id: "release-version",
    name: "Release version gate",
    program: "bun",
    args: ["run", "check:release"],
    covers: ["bun run check:release"],
  },
  {
    // docs/design/UI_RULES.md section 1, mechanically. --strict fails on any violation
    // outside the allow-list in scripts/design-lint.config.json (brand art, each with a
    // reason) and on a count that rose over the baseline; allowed and deferred hits are
    // still printed. CI does not run it yet, so it is exempt from the coverage contract.
    id: "design-lint",
    name: "Design lint (UI_RULES section 1)",
    program: "bun",
    args: ["run", "design:gate"],
    local: true,
    why: "the ratchet-and-allow-list gate for the design rules; ci.yml does not run it",
  },
  {
    id: "svelte-check",
    name: "Frontend Svelte and TypeScript check",
    program: "bun",
    args: ["run", "check"],
    covers: ["bun run check"],
  },
  {
    id: "unit-tests",
    name: "Frontend unit tests with coverage",
    program: "bun",
    args: ["run", "coverage"],
    // `coverage` is `vitest run --coverage`: the same suite CI runs as
    // `bun run test`, plus the report coverage.yml gates on.
    covers: ["bun run test", "bun run coverage"],
    produces: ["coverage/lcov.info"],
  },
  {
    id: "timezone-tests",
    name: "Frontend timezone and DST tests",
    program: "bun",
    args: ["run", "test:tz"],
    covers: ["bun run test:tz"],
  },
  {
    id: "browser-regressions",
    name: "Browser regressions (headless Chrome)",
    program: "bun",
    args: ["run", "test:browser:all"],
    covers: ["bun run test:browser:all"],
  },
  {
    id: "webkit-regressions",
    name: "Native renderer regressions (WKWebView)",
    program: "bun",
    args: ["run", "test:webkit:all"],
    covers: ["bun run test:webkit:all"],
    onlyOn: "darwin",
    why: "WKWebView is reachable only on macOS; CI runs this on its macos-latest leg",
  },
  {
    id: "build",
    name: "Frontend Vite build",
    program: "bun",
    args: ["run", "build"],
    covers: ["bun run build"],
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
    args: ["clippy", "--manifest-path", CARGO_MANIFEST, "--locked", "--all-targets", "--", "-D", "warnings"],
    covers: [`cargo clippy --manifest-path ${CARGO_MANIFEST} --locked --all-targets -- -D warnings`],
  },
  {
    id: "cargo-tests",
    name: "Rust tests with coverage",
    program: "cargo",
    // --no-fail-fast and --locked mirror ci.yml. Without --no-fail-fast the
    // run stops at the first failure and everything after it goes unrun, which
    // is the same masking this runner exists to remove, one level down.
    // Concurrency is src-tauri/.config/nextest.toml's: four test processes.
    args: [
      "llvm-cov",
      "nextest",
      "--manifest-path",
      CARGO_MANIFEST,
      "--workspace",
      "--locked",
      "--no-fail-fast",
      "--lcov",
      "--output-path",
      "lcov.info",
    ],
    covers: [
      `cargo test --manifest-path ${CARGO_MANIFEST} --locked --no-fail-fast -- --nocapture --test-threads=1`,
    ],
    produces: ["lcov.info"],
    why: "CI runs these under cargo test, one test at a time inside each binary's process; here every test is its own process, four at a time. Instrumented on an 18-core Mac (2026-11-04, runs interleaved nextest, cargo test, nextest): 908 s and 813 s against 4,300 s, line coverage 90.5% both ways. What this cannot see, CI's cargo test still does: a test that fails only beside another in the same process, and the doctests",
  },
  {
    id: "worktree-fmt",
    name: "Worktree crate format check",
    program: "cargo",
    args: ["fmt", "--manifest-path", WORKTREE_MANIFEST, "--", "--check"],
    covers: [`cargo fmt --manifest-path ${WORKTREE_MANIFEST} -- --check`],
  },
  {
    id: "worktree-clippy",
    name: "Worktree crate lint (clippy)",
    program: "cargo",
    args: ["clippy", "--manifest-path", WORKTREE_MANIFEST, "--locked", "--all-targets", "--", "-D", "warnings"],
    covers: [`cargo clippy --manifest-path ${WORKTREE_MANIFEST} --locked --all-targets -- -D warnings`],
  },
  {
    id: "worktree-tests",
    name: "Worktree crate tests",
    program: "cargo",
    args: ["test", "--manifest-path", WORKTREE_MANIFEST, "--locked", "--no-fail-fast", "--", "--test-threads=1"],
    covers: [`cargo test --manifest-path ${WORKTREE_MANIFEST} --locked --no-fail-fast -- --test-threads=1`],
  },
  {
    // The terminal suites again, every registry backed by a real
    // gitpulse-ptyd (docs/TERMINAL_HOST.md, migration step 2).
    id: "terminal-host-suites",
    name: "Terminal suites against the terminal host",
    program: "bun",
    args: ["run", "test:terminal-host"],
    covers: ["bun run test:terminal-host"],
  },
  {
    // ci.yml's Gusset engine job. It needs DevCouncil and gusset checked out
    // beside this repository, as go/go.mod's replaces do; without them the
    // script exits 2, "could not run", which fails here rather than passing.
    id: "gusset-check",
    name: "Gusset engine check",
    program: "bun",
    args: ["run", "gusset:check"],
    covers: ["bun run gusset:check"],
  },
  {
    id: "coverage-floor",
    name: "Coverage floor",
    program: "bun",
    args: ["run", "check:coverage"],
    covers: ["bun run check:coverage"],
  },
];

/** Exit code of a `--strict-tree` run whose gates passed but whose tree moved or was unreadable. */
export const EXIT_TREE_UNSTABLE = 3;

const GIT_TIMEOUT_MS = 120_000;
const OBJECT_ID = /^[0-9a-f]{40}$|^[0-9a-f]{64}$/;

/**
 * @typedef {{ status: number | null, stdout: string, stderr: string, error?: string }} GitResult
 * @typedef {(args: string[], where: { cwd: string, env: NodeJS.ProcessEnv }) => GitResult} GitRunner
 * @type {GitRunner}
 */
function runGit(args, { cwd, env }) {
  const result = spawnSync("git", args, {
    cwd,
    env,
    encoding: "utf8",
    timeout: GIT_TIMEOUT_MS,
    maxBuffer: 64 * 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  });
  return {
    status: result.status,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
    error: result.error ? result.error.message : undefined,
  };
}

/**
 * Git's tree id for the working tree exactly as it is now: tracked changes and
 * untracked files included, ignored files excluded. A private copy of the real
 * index is used (so Git's stat cache is reused and the real index is never
 * written); with no index to copy, `git add` builds one. The temporary index is
 * removed on success and on failure. Anything that goes wrong, including being
 * outside a repository, yields `{ ok: false, reason }` and never an id.
 *
 * @typedef {{ ok: true, id: string } | { ok: false, reason: string }} TreeReading
 * @param {string} root the repository's top directory
 * @param {{ run?: GitRunner, tmpRoot?: string, env?: NodeJS.ProcessEnv }} [options]
 * @returns {TreeReading}
 */
export function workingTreeId(root, options = {}) {
  const { run = runGit, tmpRoot = tmpdir(), env = process.env } = options;
  /** @type {TreeReading} */
  let result;
  /** @type {string | undefined} */
  let scratch;
  /** @param {string[]} args @param {NodeJS.ProcessEnv} stepEnv */
  const git = (args, stepEnv) => {
    const out = run(args, { cwd: root, env: stepEnv });
    const label = args.find((arg) => ["rev-parse", "add", "write-tree"].includes(arg));
    if (out.error) throw new Error(`git ${label}: ${out.error}`);
    if (out.status !== 0) {
      const why = out.stderr.trim().split("\n")[0] || `exit ${out.status}`;
      throw new Error(`git ${label} failed: ${why}`);
    }
    return out.stdout;
  };
  try {
    const realIndex = path.resolve(root, git(["rev-parse", "--git-path", "index"], env).trim());
    scratch = mkdtempSync(path.join(tmpRoot, "ci-local-tree-"));
    const tmpIndex = path.join(scratch, "index");
    if (existsSync(realIndex)) {
      copyFileSync(realIndex, tmpIndex);
      // Keep the real index's timestamp: Git decides whether an entry is
      // "racily clean" by comparing it with the index file's mtime.
      const { atime, mtime } = statSync(realIndex);
      utimesSync(tmpIndex, atime, mtime);
    }
    const privateEnv = { ...env, GIT_INDEX_FILE: tmpIndex };
    git(["-c", "core.fsmonitor=false", "add", "-A", "--", ":/"], privateEnv);
    const id = git(["-c", "core.fsmonitor=false", "write-tree"], privateEnv).trim();
    if (!OBJECT_ID.test(id)) throw new Error(`git write-tree printed ${JSON.stringify(id)}, not an object id`);
    result = { ok: true, id };
  } catch (error) {
    result = { ok: false, reason: error instanceof Error ? error.message : String(error) };
  }
  if (scratch !== undefined) {
    try {
      rmSync(scratch, { recursive: true, force: true });
    } catch (error) {
      const cleanup = `could not remove the temporary index: ${error instanceof Error ? error.message : String(error)}`;
      result = { ok: false, reason: result.ok ? cleanup : `${result.reason}; ${cleanup}` };
    }
  }
  return result;
}

/**
 * What two readings of the working tree, taken before the first gate and after
 * the last, say about the run.
 *
 * @typedef {{ state: "stable", id: string }
 *   | { state: "changed", before: string, after: string }
 *   | { state: "unknown", reasons: string[] }} TreeVerdict
 * @param {TreeReading} before @param {TreeReading} after @returns {TreeVerdict}
 */
export function compareTrees(before, after) {
  if (before.ok && after.ok) {
    return before.id === after.id
      ? { state: "stable", id: before.id }
      : { state: "changed", before: before.id, after: after.id };
  }
  /** @type {string[]} */
  const reasons = [];
  if (!before.ok) reasons.push(`before the first gate: ${before.reason}`);
  if (!after.ok) reasons.push(`after the last gate: ${after.reason}`);
  return { state: "unknown", reasons };
}

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
 * Run every gate, continuing past failures. With `root`, each gate's
 * `produces` reports under it are removed before the gate runs; `main` passes
 * the repository root, and a caller without one leaves files alone.
 *
 * @param {Gate[]} gates
 * @param {{ run?: (gate: Gate) => { ok: boolean, detail: string, ms: number },
 *           platform?: NodeJS.Platform, log?: (line: string) => void,
 *           root?: string }} [options]
 */
export function runGates(gates, options = {}) {
  const { run = runGate, platform = process.platform, log = console.log, root } = options;
  const outcomes = [];
  for (const gate of gates) {
    const skipped = skipReason(gate, platform);
    if (skipped) {
      log(`\n— SKIP ${gate.name} (${skipped})`);
      outcomes.push({ gate, status: "skipped", detail: skipped, ms: 0 });
      continue;
    }
    log(`\n▶ ${gate.name}\n  ${commandLine(gate)}`);
    if (root !== undefined) {
      for (const report of gate.produces ?? []) rmSync(path.join(root, report), { force: true });
    }
    const { ok, detail, ms } = run(gate);
    outcomes.push({ gate, status: ok ? "passed" : "failed", detail, ms });
    if (!ok) log(`✗ ${gate.name} failed (${detail}) — continuing so the rest still report`);
  }
  return outcomes;
}

/**
 * @param {ReturnType<typeof runGates>} outcomes
 * @param {TreeVerdict} [tree] omitted by callers that did not read the tree
 */
export function summarize(outcomes, tree) {
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
  if (tree) lines.push("", ...treeLines(tree));
  lines.push("");
  return lines.join("\n");
}

/** @param {TreeVerdict} tree @returns {string[]} */
function treeLines(tree) {
  if (tree.state === "stable") {
    return [`  tested tree: ${tree.id}`, "  (identical when the first gate started and when the last one ended)"];
  }
  if (tree.state === "changed") {
    return [
      "  TREE CHANGED DURING THE RUN",
      `    at start: ${tree.before}`,
      `    at end:   ${tree.after}`,
      "  The working tree changed while the gates ran, so this result does not describe either tree.",
      "  RESULT: INCONCLUSIVE, not a clean pass. Re-run when the tree is quiet.",
    ];
  }
  return [
    "  tree: could not be read, so which files were tested is not recorded",
    ...tree.reasons.map((reason) => `    ${reason}`),
  ];
}

/**
 * The paste-able evidence line, or null. It exists only when every gate in
 * GATES ran and passed and the tree was read twice with the same answer.
 *
 * @param {ReturnType<typeof runGates>} outcomes @param {TreeVerdict} tree
 * @returns {string | null}
 */
export function testedTreeLine(outcomes, tree) {
  const everyGate = outcomes.length === GATES.length && outcomes.every((o, i) => o.gate.id === GATES[i].id);
  if (!everyGate || !outcomes.every((o) => o.status === "passed") || tree.state !== "stable") return null;
  return `Tested-tree: ${tree.id}`;
}

/**
 * Read the tree, run the gates, read the tree again, print the summary, and
 * return the exit code.
 *
 * @param {{ strictTree?: boolean, gates?: Gate[], readTree?: () => TreeReading,
 *           run?: (gate: Gate) => { ok: boolean, detail: string, ms: number },
 *           platform?: NodeJS.Platform, log?: (line: string) => void, root?: string }} [options]
 * @returns {number}
 */
export function execute(options = {}) {
  const {
    strictTree = false,
    gates = GATES,
    readTree = () => workingTreeId(REPO_ROOT),
    run,
    platform,
    log = console.log,
    root,
  } = options;
  const before = readTree();
  const outcomes = runGates(gates, { run, platform, log, root });
  const after = readTree();
  const tree = compareTrees(before, after);
  log(summarize(outcomes, tree));
  const tested = testedTreeLine(outcomes, tree);
  if (tested) log(tested);
  if (outcomes.some((o) => o.status === "failed")) return 1;
  return strictTree && tree.state !== "stable" ? EXIT_TREE_UNSTABLE : 0;
}

function usage() {
  return formatUsage({
    name: "ci-local",
    summary: "Run the local gates (a subset of CI), without one failure hiding the rest.",
    flags: [
      { flag: "--list", description: "Print the gates and exit" },
      {
        flag: "--strict-tree",
        description: "Exit 3 when every gate passed but the working tree changed during the run or could not be read",
      },
      { flag: "--help, -h", description: "Show this message" },
    ],
    exits:
      "0 every gate that ran passed · 1 at least one gate failed · 2 unknown option · " +
      "3 (--strict-tree only) gates passed but the working tree changed during the run or could not be read",
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
  const unknown = argv.find((a) => !["--list", "--strict-tree", "--help", "-h"].includes(a));
  if (unknown) {
    console.error(`FAIL: unknown option ${JSON.stringify(unknown)}\n`);
    console.error(usage());
    return 2;
  }
  return execute({ strictTree: argv.includes("--strict-tree"), root: REPO_ROOT });
}

if (process.argv[1]?.endsWith("ci-local.mjs")) {
  process.exitCode = main();
}
