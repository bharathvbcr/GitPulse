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
 * Exit codes: 0 every gate that ran passed · 1 at least one gate failed ·
 * 3 every gate passed but a coverage build was not removed (never for a build
 * kept on request).
 */

import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
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
 * `reads` lists the reports a gate reads (repository-relative), each of which
 * an earlier gate `produces`. It is what says when a producer's leftovers are
 * no longer needed: the last gate that reads what it made has run.
 *
 * `scratch` lists the build output a gate leaves behind that nothing needs once
 * its reports have been read. The runner deletes it (see `cleanScratch`), by
 * `kind` rather than by gate. Today that is the instrumented build `cargo
 * llvm-cov` writes to `<target>/llvm-cov-target`: about 50 GiB per run, which on
 * 2026-11-15 took two sessions' free disk from 99 to 45 GiB in twenty minutes.
 *
 * @typedef {{ kind: "cargo-llvm-cov", manifest: string }} Scratch
 * @typedef {{
 *   id: string, name: string, program: string, args: string[],
 *   covers?: string[], onlyOn?: NodeJS.Platform, local?: boolean, why?: string,
 *   produces?: string[], reads?: string[], scratch?: Scratch[],
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
    scratch: [{ kind: "cargo-llvm-cov", manifest: CARGO_MANIFEST }],
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
    // scripts/check-coverage-floor.mjs's defaults: the frontend and the Rust report.
    reads: ["coverage/lcov.info", "lcov.info"],
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
    exitCode: result.error ? null : result.status,
  };
}

/**
 * Whether the keep opt-out is on: only "1", "true" or "yes" (any case). Anything
 * else, including "0", "false", "no" and the empty string, leaves it off, so a
 * stray value cannot silently keep 50 GiB.
 *
 * @param {string | undefined} value
 */
export function wantsKeep(value) {
  return value !== undefined && ["1", "true", "yes"].includes(value.toLowerCase());
}

/** Set to 1 (or true/yes) to keep the coverage build for an incremental rebuild. */
export const KEEP_COVERAGE_BUILD_ENV = "CI_LOCAL_KEEP_COVERAGE_BUILD";

/** Cargo drops one of these at the root of every target directory it creates. */
const CARGO_TARGET_MARKERS = ["CACHEDIR.TAG", ".rustc_info.json"];
const COVERAGE_BUILD_NAME = "llvm-cov-target";

/**
 * Exit codes that mean the owning gate never got as far as running cargo:
 * 75 is the machine's cargo slot gate giving up (~/.claude-bin/cargo), 126 and
 * 127 are "found but not executable" and "not found".
 */
const NOT_BUILT_EXIT_CODES = [75, 126, 127];

/** `ci:local` exit code for "every gate passed but a coverage build was not removed". */
export const EXIT_SCRATCH_NOT_REMOVED = 3;

/**
 * What the runner needs to delete scratch output safely. Cleanup is opt-in:
 * `runGates` does nothing about `scratch` unless the caller passes this, and
 * only `main` passes the real one.
 *
 * @typedef {{ targetDirectory: string, buildDirectory: string }} CargoDirectories
 * @typedef {{
 *   env: Record<string, string | undefined>,
 *   cargoMetadata: (manifest: string) => CargoDirectories,
 *   home?: string,
 *   remove?: (dir: string) => void,
 *   listProcesses?: () => string,
 *   processStart?: (pid: number) => string | null,
 * }} ScratchContext
 * @typedef {"removed" | "absent" | "not-found" | "kept" | "refused" | "failed"} CleanupStatus
 * @typedef {{
 *   kind: string, status: CleanupStatus,
 *   dir: string | null, bytes: number | null, detail: string, note?: string,
 * }} CleanupResult
 * @typedef {{
 *   file: string | null, token: string | null, otherPid: number | null,
 *   note: string | null, error: string | null,
 * }} RunLock
 * @typedef {{ id: string, state: string }} Fingerprint
 * @typedef {{ dir: string, named: boolean, optional: boolean, before: Fingerprint | null, lock: RunLock }} SnapshotCandidate
 * @typedef {{
 *   scratch: Scratch, cargo: CargoDirectories | null, candidates: SnapshotCandidate[],
 *   failure: CleanupResult | null,
 * }} ScratchSnapshot
 * @typedef {{ ran: boolean, passed: boolean, detail: string }} OwnerEvidence
 */

/**
 * Where `cargo metadata` says cargo builds, for this manifest. The same source
 * cargo-llvm-cov reads (`metadata.target_directory`, `build_directory`), so
 * `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR` and `.cargo/config.toml` are all
 * honoured without this file knowing about them. Not a build; it compiles nothing.
 *
 * It is not routed through the machine's cargo slot gate (`~/.claude-bin/cargo`
 * execs the real cargo for every subcommand but the build ones), took 0.1 s on
 * 2026-11-15, and is bounded at 60 s regardless.
 *
 * @typedef {(args: string[]) => { error?: Error, status: number | null, stdout: string }} CargoRunner
 *
 * @param {string} manifest repository-relative Cargo.toml
 * @param {CargoRunner} [runCargo] the process seam
 * @returns {CargoDirectories}
 */
export function cargoMetadata(manifest, runCargo = spawnCargo) {
  const result = runCargo(["metadata", "--no-deps", "--format-version", "1", "--manifest-path", manifest]);
  if (result.error) throw new Error(`cargo metadata could not run: ${result.error.message}`);
  if (result.status !== 0) throw new Error(`cargo metadata exited ${result.status}`);
  /** @type {unknown} */
  const parsed = JSON.parse(result.stdout);
  /** @param {string} key @returns {string | null} */
  const field = (key) => {
    const value = typeof parsed === "object" && parsed !== null ? Reflect.get(parsed, key) : undefined;
    return typeof value === "string" && value !== "" ? value : null;
  };
  const targetDirectory = field("target_directory");
  if (targetDirectory === null) throw new Error("cargo metadata reported no target_directory");
  return { targetDirectory, buildDirectory: field("build_directory") ?? targetDirectory };
}

/** The real context `main` hands to `runGates`: the process environment, cargo itself, this user's home. */
export function productionScratchContext() {
  /** @type {ScratchContext} */
  const context = { env: process.env, cargoMetadata, home: homedir(), listProcesses: psSnapshot, processStart: psStart };
  return context;
}

/** @type {CargoRunner} */
function spawnCargo(args) {
  const result = spawnSync("cargo", args, {
    cwd: REPO_ROOT,
    encoding: "utf8",
    timeout: 60_000,
    maxBuffer: 64 * 1024 * 1024,
    shell: process.platform === "win32",
  });
  return { error: result.error, status: result.status, stdout: result.stdout ?? "" };
}

/** @param {string} dir @returns {string | null} */
function realpathOrNull(dir) {
  try {
    return realpathSync(dir);
  } catch {
    return null;
  }
}

/** @param {string} child @param {string} parent */
function isInside(child, parent) {
  const relative = path.relative(parent, child);
  return relative !== "" && !relative.startsWith("..") && !path.isAbsolute(relative);
}

/**
 * Disk bytes under `dir`, without following symlinks. Null when it cannot be
 * read: an unknown size is reported as unknown, never as zero.
 *
 * @param {string} dir
 * @returns {number | null}
 */
export function measureBytes(dir) {
  const seen = new Set();
  let total = 0;
  /** @param {string} current */
  const walk = (current) => {
    const stat = lstatSync(current);
    if (stat.nlink > 1 && !stat.isDirectory()) {
      const key = `${stat.dev}:${stat.ino}`;
      if (seen.has(key)) return;
      seen.add(key);
    }
    total += typeof stat.blocks === "number" ? stat.blocks * 512 : stat.size;
    if (stat.isDirectory()) {
      for (const entry of readdirSync(current)) walk(path.join(current, entry));
    }
  };
  try {
    walk(dir);
    return total;
  } catch {
    return null;
  }
}

/** @param {number} bytes */
function formatBytes(bytes) {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
}

class RefusedError extends Error {}

/**
 * An environment variable the way cargo-llvm-cov reads it: v0.9.1 `src/env.rs`
 * `var()` returns `None` for an empty value, exactly as for an unset one.
 *
 * @param {Record<string, string | undefined>} env
 * @param {string} name
 * @returns {string | undefined}
 */
function envValue(env, name) {
  const value = env[name];
  return value === undefined || value === "" ? undefined : value;
}

/** @param {string} value */
function hasParentComponent(value) {
  return value.split(/[\\/]/).includes("..");
}

/**
 * cargo-llvm-cov joins the raw string onto the current directory and the OS
 * resolves `..` through whatever symlinks are on the way, so `link/../x` is
 * `<link target's parent>/x`. `path.resolve` collapses `..` as text and would
 * name a different directory. Rather than emulate the OS and risk deleting the
 * wrong one, a value with a `..` component is refused.
 *
 * @param {string} variable
 * @param {string} value
 */
function refuseParentComponent(variable, value) {
  if (hasParentComponent(value)) {
    throw new RefusedError(
      `${variable}=${JSON.stringify(value)} has a ".." component: cargo-llvm-cov lets the OS resolve it through symlinks, ` +
        "which this runner will not guess at, so the directory it names cannot be identified",
    );
  }
}

/**
 * The directories one `cargo llvm-cov nextest --manifest-path <manifest>` run
 * writes its instrumented build to.
 *
 * Read from cargo-llvm-cov v0.9.1, src/cargo.rs `Workspace::new` and src/env.rs
 * `var` (the version installed on this machine): an empty variable counts as
 * unset. When `CARGO_LLVM_COV_TARGET_DIR` is set the target directory is the
 * current directory joined with it, and the build directory is the current
 * directory joined with `CARGO_LLVM_COV_BUILD_DIR` if that is set (else the
 * target directory itself); `CARGO_LLVM_COV_BUILD_DIR` is not read otherwise.
 * When neither is set they are `<metadata target_directory>/llvm-cov-target` and
 * `<metadata build_directory>/llvm-cov-target`. The build directory takes effect
 * only when cargo's `build.build-dir` is configured. `cargo metadata` shows that
 * as `build_directory` differing from `target_directory`, but a configuration
 * that points at the same path is invisible there, so with the target named and
 * the build directory named too and no difference in metadata, the build
 * directory is an `optional` candidate: removed only if this run wrote it, and
 * otherwise left alone without a word. The gate passes no `--target-dir`, and
 * runs with the repository root as its current directory.
 *
 * @param {string} root
 * @param {ScratchContext} context
 * @param {CargoDirectories} cargo
 * @returns {{ dir: string, named: boolean, optional: boolean }[]} named: spelled out by an environment variable
 */
function coverageBuildCandidates(root, context, cargo) {
  const { env } = context;
  /** @type {{ dir: string, named: boolean, optional: boolean }[]} */
  const found = [];
  /** @param {string} dir @param {boolean} named @param {boolean} optional */
  const add = (dir, named, optional) => {
    if (!found.some((f) => f.dir === dir)) found.push({ dir, named, optional });
  };
  const target = envValue(env, "CARGO_LLVM_COV_TARGET_DIR");
  const build = envValue(env, "CARGO_LLVM_COV_BUILD_DIR");
  const buildConfigured = cargo.buildDirectory !== cargo.targetDirectory;
  if (target !== undefined) {
    refuseParentComponent("CARGO_LLVM_COV_TARGET_DIR", target);
    add(path.resolve(root, target), true, false);
    if (build !== undefined) {
      refuseParentComponent("CARGO_LLVM_COV_BUILD_DIR", build);
      add(path.resolve(root, build), true, !buildConfigured);
    }
  } else {
    add(path.join(cargo.targetDirectory, COVERAGE_BUILD_NAME), false, false);
    if (buildConfigured) add(path.join(cargo.buildDirectory, COVERAGE_BUILD_NAME), false, false);
  }
  return found;
}

/** @param {string} target */
function lstatOrNull(target) {
  try {
    return lstatSync(target);
  } catch {
    return null;
  }
}

/**
 * What a directory looks like right now: its device and inode, its own mtime and
 * its top-level entries with theirs. Two equal fingerprints mean nothing was
 * added, removed or rewritten at the top level between them, which is how a
 * build directory that predates a run, and that the run never touched, is told
 * from one the run wrote.
 *
 * @param {string} dir
 * @returns {Fingerprint | null} null when nothing is there
 */
function fingerprintOf(dir) {
  const stat = lstatOrNull(dir);
  if (stat === null) return null;
  const id = `${stat.dev}:${stat.ino}`;
  if (!stat.isDirectory()) return { id, state: `${id}:not-a-directory:${stat.mtimeMs}` };
  let entries;
  try {
    entries = readdirSync(dir)
      .sort()
      .map((name) => {
        const entry = lstatOrNull(path.join(dir, name));
        return `${name}:${entry === null ? "gone" : `${entry.ino}:${entry.mtimeMs}`}`;
      });
  } catch {
    entries = ["unreadable"];
  }
  return { id, state: `${id}:${stat.mtimeMs}:${entries.join("|")}` };
}

/**
 * `ps` as the live-use check reads it. Throws when it cannot be taken: a
 * snapshot that could not be read is not evidence that nothing is using a directory.
 *
 * @returns {string}
 */
function psSnapshot() {
  const result = spawnSync("ps", ["-axo", "pid=,command="], {
    encoding: "utf8",
    timeout: 30_000,
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.error) throw new Error(`ps could not run: ${result.error.message}`);
  if (result.status !== 0) throw new Error(`ps exited ${result.status}`);
  return result.stdout ?? "";
}

/**
 * Every spelling of `dir` a process might use: as given, as resolved, and each
 * with and without the `/private` prefix macOS puts on `/tmp`, `/var` and `/etc`.
 *
 * @param {string[]} dirs
 * @returns {string[]}
 */
function pathSpellings(dirs) {
  const out = new Set();
  for (const dir of dirs) {
    out.add(dir);
    if (dir.startsWith("/private/")) out.add(dir.slice("/private".length));
    else if (dir.startsWith("/")) out.add(`/private${dir}`);
  }
  return [...out];
}

/**
 * The first process line that names one of `dirs`, other than this runner, in
 * any spelling. The match must sit on path boundaries: `/x/llvm-cov-target` is
 * not in `/other/x/llvm-cov-target/a` (a path character before it) nor in
 * `/x/llvm-cov-target-2` (a name character after it), but is in
 * `--target-dir=/x/llvm-cov-target/debug`. Arguments and the executable path are
 * both in `command`; a process's working directory is not, and is not checked.
 *
 * @param {string} listing
 * @param {string[]} dirs
 * @returns {string | null}
 */
function processNaming(listing, dirs) {
  const spellings = pathSpellings(dirs);
  for (const line of listing.split("\n")) {
    const pid = /^\s*(\d+)\s/.exec(line)?.[1];
    if (pid === String(process.pid)) continue;
    for (const dir of spellings) {
      let from = line.indexOf(dir);
      while (from !== -1) {
        const before = line[from - 1];
        const after = line[from + dir.length];
        const startsPath = before === undefined || !/[\w.\-/]/.test(before);
        const endsPath = after === undefined || !/[\w.-]/.test(after);
        if (startsPath && endsPath) return line.trim();
        from = line.indexOf(dir, from + 1);
      }
    }
  }
  return null;
}

/**
 * A process's start time as `ps` prints it, or null when no such process
 * exists. Throws when `ps` cannot be run: that is not "gone".
 *
 * @param {number} pid
 * @returns {string | null}
 */
function psStart(pid) {
  const result = spawnSync("ps", ["-o", "lstart=", "-p", String(pid)], { encoding: "utf8", timeout: 30_000 });
  if (result.error) throw new Error(`ps could not run: ${result.error.message}`);
  const out = (result.stdout ?? "").trim();
  return result.status === 0 && out !== "" ? out : null;
}

const LOCK_SUFFIX = ".ci-local.lock";

/**
 * Claim a coverage build directory for this run by creating, exclusively, a lock
 * file beside it that records this process's pid and start time. A lock whose
 * process is alive and started when the lock says is another run's: it is not
 * touched, and the directory is not this run's to delete. A lock whose process
 * is gone, or is a different process that reused the pid, is stale and replaced.
 * Never throws; a lock that could not be taken says why.
 *
 * @param {string} dir
 * @param {ScratchContext} context
 * @returns {RunLock}
 */
function acquireLock(dir, context) {
  /** @type {RunLock} */
  const lock = { file: null, token: null, otherPid: null, note: null, error: null };
  const startOf = context.processStart ?? psStart;
  try {
    mkdirSync(path.dirname(dir), { recursive: true });
    const resolved = path.join(realpathSync(path.dirname(dir)), path.basename(dir));
    const file = `${resolved}${LOCK_SUFFIX}`;
    const token = randomUUID();
    const body = JSON.stringify({ pid: process.pid, start: startOf(process.pid) ?? "unknown", dir: resolved, token });
    for (let attempt = 0; attempt < 2; attempt += 1) {
      try {
        writeFileSync(file, body, { flag: "wx" });
        lock.file = file;
        lock.token = token;
        return lock;
      } catch (error) {
        if (!(error instanceof Error) || Reflect.get(error, "code") !== "EEXIST") throw error;
      }
      /** @type {unknown} */
      let held;
      try {
        held = JSON.parse(readFileSync(file, "utf8"));
      } catch {
        lock.error = `an unreadable lock file is there (${file}); remove it if no other ci:local run is going`;
        return lock;
      }
      const pid = Number(Reflect.get(Object(held), "pid"));
      const start = Reflect.get(Object(held), "start");
      const current = Number.isInteger(pid) && pid > 0 ? startOf(pid) : null;
      if (current !== null && (start === "unknown" || current === start)) {
        lock.otherPid = pid;
        return lock;
      }
      unlinkSync(file);
      lock.note = `replaced a stale run lock left by pid ${Number.isInteger(pid) ? pid : "?"}`;
    }
    lock.error = "lost a race for the run lock";
  } catch (error) {
    lock.error = `could not take the run lock beside it: ${error instanceof Error ? error.message : String(error)}`;
  }
  return lock;
}

/** Remove the lock this run took, and only that one. @param {RunLock} lock */
function releaseLock(lock) {
  if (lock.file === null || lock.token === null) return;
  try {
    const held = JSON.parse(readFileSync(lock.file, "utf8"));
    if (Reflect.get(Object(held), "token") === lock.token) unlinkSync(lock.file);
  } catch {
    // Already gone, or replaced by another run: not ours to remove.
  }
}

/**
 * Whether `dir` may be deleted as a coverage build, by where it is and what it
 * is called, or the reason it may not. Checked on the real path of its parent,
 * so a symlink on the way cannot redirect the delete.
 *
 * @param {{ dir: string, named: boolean }} candidate
 * @param {string} root
 * @param {ScratchContext} context
 * @param {CargoDirectories} cargo
 * @returns {{ ok: true, real: string, outside: boolean } | { ok: false, reason: string }}
 */
function vet(candidate, root, context, cargo) {
  const { dir, named } = candidate;
  const realParent = realpathOrNull(path.dirname(dir));
  if (realParent === null) return { ok: false, reason: "its parent directory does not exist" };
  const real = path.join(realParent, path.basename(dir));
  const realRoot = realpathOrNull(root) ?? root;
  const home = realpathOrNull(context.home ?? homedir()) ?? context.home ?? homedir();
  if (real === path.parse(real).root) return { ok: false, reason: "it is the filesystem root" };
  if (real === realRoot || isInside(realRoot, real)) {
    return { ok: false, reason: "it is, or contains, the repository" };
  }
  if (real === home || isInside(home, real)) return { ok: false, reason: "it is, or contains, the home directory" };
  for (const regular of [cargo.targetDirectory, cargo.buildDirectory]) {
    const realRegular = realpathOrNull(regular) ?? regular;
    if (real === realRegular || isInside(realRegular, real)) {
      return { ok: false, reason: "it is, or contains, cargo's regular target directory" };
    }
  }
  const outside = !isInside(real, realRoot);
  const leafIsCoverage = path.basename(real) === COVERAGE_BUILD_NAME;
  const marked = CARGO_TARGET_MARKERS.some((marker) => lstatOrNull(path.join(real, marker)) !== null);
  if (outside) {
    // Outside the repository only a path the environment names for this build is ours to delete.
    const targetEnv = envValue(context.env, "CARGO_TARGET_DIR");
    let viaCargoTargetDir = false;
    if (!named && targetEnv !== undefined) {
      if (hasParentComponent(targetEnv)) {
        return { ok: false, reason: `CARGO_TARGET_DIR=${JSON.stringify(targetEnv)} has a ".." component, so where it points cannot be identified` };
      }
      viaCargoTargetDir = realpathOrNull(path.resolve(root, targetEnv)) === realParent;
    }
    if (!named && !viaCargoTargetDir) {
      return {
        ok: false,
        reason: "it is outside the repository and is not named by CARGO_TARGET_DIR or CARGO_LLVM_COV_TARGET_DIR",
      };
    }
    if (!leafIsCoverage) {
      return { ok: false, reason: `outside the repository only a directory named ${COVERAGE_BUILD_NAME} is deleted, whatever names it` };
    }
    if (!marked) {
      return { ok: false, reason: `it holds none of ${CARGO_TARGET_MARKERS.join(", ")}, so it is not recognisable as a cargo build directory` };
    }
  } else if (!leafIsCoverage) {
    if (!marked) {
      return {
        ok: false,
        reason: `it is not named ${COVERAGE_BUILD_NAME} and holds none of ${CARGO_TARGET_MARKERS.join(", ")}, so it is not recognisable as a cargo build directory`,
      };
    }
    // A regular cargo target holds profile directories with a .fingerprint inside; whatever names it, it is not the coverage build.
    if (["debug", "release"].some((profile) => lstatOrNull(path.join(real, profile, ".fingerprint")) !== null)) {
      return { ok: false, reason: `it is not named ${COVERAGE_BUILD_NAME} and looks like a regular cargo target directory (debug or release with a .fingerprint)` };
    }
  }
  return { ok: true, real, outside };
}

/**
 * Resolve where a scratch entry's output will go and record what is there now,
 * before its gate runs. Never throws.
 *
 * @param {Scratch} scratch
 * @param {string} root repository root
 * @param {ScratchContext} context
 * @returns {ScratchSnapshot}
 */
export function snapshotScratch(scratch, root, context) {
  /** @param {CleanupStatus} status @param {string} detail @returns {ScratchSnapshot} */
  const failed = (status, detail) => ({
    scratch,
    cargo: null,
    candidates: [],
    failure: { kind: scratch.kind, status, dir: null, bytes: null, detail },
  });
  if (scratch.kind !== "cargo-llvm-cov") return failed("refused", `unknown scratch kind ${JSON.stringify(scratch.kind)}`);
  try {
    const cargo = context.cargoMetadata(scratch.manifest);
    const candidates = coverageBuildCandidates(root, context, cargo).map((candidate) => ({
      ...candidate,
      before: fingerprintOf(candidate.dir),
      lock: acquireLock(candidate.dir, context),
    }));
    return { scratch, cargo, candidates, failure: null };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return failed(error instanceof RefusedError ? "refused" : "failed", `could not identify the coverage build directory: ${message}`);
  }
}

/**
 * Delete what one snapshotted scratch entry names, or say precisely why it was
 * not. Never throws, and never reports a removal it did not confirm: after the
 * delete the path is looked at again.
 *
 * A directory is deleted only if this run wrote it: it did not exist before the
 * owning gate, or it differs from how it was, and the gate got as far as running
 * cargo. A directory that predates the run unchanged is somebody else's.
 *
 * @param {ScratchSnapshot} snapshot
 * @param {string} root repository root
 * @param {ScratchContext} context
 * @param {OwnerEvidence} evidence
 * @returns {CleanupResult[]}
 */
export function cleanScratch(snapshot, root, context, evidence) {
  try {
    return cleanCandidates(snapshot, root, context, evidence);
  } finally {
    for (const candidate of snapshot.candidates) releaseLock(candidate.lock);
  }
}

/**
 * @param {ScratchSnapshot} snapshot
 * @param {string} root
 * @param {ScratchContext} context
 * @param {OwnerEvidence} evidence
 * @returns {CleanupResult[]}
 */
function cleanCandidates(snapshot, root, context, evidence) {
  const kind = snapshot.scratch.kind;
  /** @param {CleanupStatus} status @param {string | null} dir @param {string} detail @param {number | null} [bytes] @returns {CleanupResult} */
  const result = (status, dir, detail, bytes = null) => ({ kind, status, dir, bytes, detail });
  if (wantsKeep(context.env[KEEP_COVERAGE_BUILD_ENV])) {
    return [result("kept", null, `${KEEP_COVERAGE_BUILD_ENV} is set; the coverage build is left in place`)];
  }
  const { cargo } = snapshot;
  if (snapshot.failure !== null || cargo === null) {
    return [snapshot.failure ?? result("failed", null, "could not identify the coverage build directory")];
  }
  return snapshot.candidates.map((candidate) => {
    const decided = cleanCandidate(candidate);
    return candidate.lock.note === null ? decided : { ...decided, note: candidate.lock.note };
  });

  /** @param {SnapshotCandidate} candidate @returns {CleanupResult} */
  function cleanCandidate(candidate) {
    if (cargo === null) return result("failed", null, "could not identify the coverage build directory");
    const { dir } = candidate;
    const leaf = lstatOrNull(dir);
    if (leaf === null) {
      if (candidate.optional) return result("absent", dir, "not used by this run: nothing is there");
      if (evidence.passed) {
        return result(
          "not-found",
          dir,
          "the owning gate passed, so its coverage build exists somewhere, but nothing is here: the runner looked in the wrong place and the build is still on disk",
        );
      }
      return result("absent", dir, "nothing to remove: the directory does not exist");
    }
    if (candidate.lock.otherPid !== null) {
      return result("refused", dir, `another ci:local run (pid ${candidate.lock.otherPid}) is using it`);
    }
    if (candidate.lock.error !== null) {
      return result("refused", dir, `${candidate.lock.error}, so another run cannot be ruled out`);
    }
    if (leaf.isSymbolicLink()) return result("refused", dir, "it is a symlink, which is never followed");
    if (!leaf.isDirectory()) return result("refused", dir, "it is not a directory");
    const verdict = vet(candidate, root, context, cargo);
    if (!verdict.ok) return result("refused", dir, verdict.reason);
    if (!evidence.ran) {
      return result("refused", dir, `this run's gate did not run cargo to completion (${evidence.detail}), so a directory here is not this run's build`);
    }
    const now = fingerprintOf(dir);
    if (now === null) return result("absent", dir, "nothing to remove: the directory does not exist");
    if (candidate.before !== null && candidate.before.state === now.state) {
      if (candidate.optional) return result("absent", dir, "not used by this run: it did not change while the gate ran");
      return result("refused", dir, "it predates this run and did not change while the gate ran, so it is not this run's build");
    }
    // Sized before the last checks, so the checks and the delete sit close together.
    const bytes = measureBytes(verdict.real);
    // A second guard for a bare `cargo llvm-cov` that holds no run lock, inside the repository as well as outside it.
    try {
      const using = processNaming((context.listProcesses ?? psSnapshot)(), [dir, verdict.real]);
      if (using !== null) return result("refused", dir, `a live process names it, so another run may be using it: ${using.slice(0, 200)}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return result("refused", dir, `it could not be shown to be unused: ${message}`);
    }
    const last = lstatOrNull(verdict.real);
    if (last === null || last.isSymbolicLink() || !last.isDirectory() || `${last.dev}:${last.ino}` !== now.id) {
      return result("refused", dir, "it changed identity between the checks and the delete");
    }
    try {
      (context.remove ?? ((target) => rmSync(target, { recursive: true, force: false, maxRetries: 3 })))(verdict.real);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return result("failed", dir, `removal failed: ${message}`, bytes);
    }
    if (lstatOrNull(verdict.real) !== null) return result("failed", dir, "removal reported success but the directory is still there", bytes);
    return result("removed", dir, "removed", bytes);
  }
}

/** Whether a cleanup left a coverage build on disk that should have gone, or could not be shown to be. */
export function cleanupProblem(/** @type {CleanupResult} */ cleanup) {
  return cleanup.status === "refused" || cleanup.status === "failed" || cleanup.status === "not-found";
}

/** @param {CleanupResult} cleanup */
function describeCleanup(cleanup) {
  const where = cleanup.dir ?? cleanup.kind;
  const note = cleanup.note === undefined ? "" : ` [${cleanup.note}]`;
  switch (cleanup.status) {
    case "removed":
      return `removed ${where}${cleanup.bytes === null ? " (size unknown)" : `, freed ${formatBytes(cleanup.bytes)}`}${note}`;
    case "absent":
      return `${where}: ${cleanup.detail}${note}`;
    case "kept":
      return `kept: ${cleanup.detail}${note}`;
    default:
      return `COVERAGE BUILD NOT REMOVED (${cleanup.status}): ${where}: ${cleanup.detail}${note}`;
  }
}

/**
 * For each gate that leaves scratch output, the index of the gate after which
 * it can go: the last later gate that reads one of the reports it produces, or
 * the gate itself when none does.
 *
 * @param {Gate[]} gates
 * @returns {Map<number, number>} owner index to cleanup index
 */
export function cleanupPoints(gates) {
  /** @type {Map<number, number>} */
  const points = new Map();
  gates.forEach((gate, owner) => {
    if (!gate.scratch?.length) return;
    const made = new Set(gate.produces ?? []);
    let last = owner;
    for (let later = owner + 1; later < gates.length; later += 1) {
      if ((gates[later].reads ?? []).some((report) => made.has(report))) last = later;
    }
    points.set(owner, last);
  });
  return points;
}

/**
 * Run every gate, continuing past failures. With `root`, each gate's
 * `produces` reports under it are removed before the gate runs; `main` passes
 * the repository root, and a caller without one leaves files alone.
 *
 * With `root` and `scratch`, a gate's `scratch` output is resolved and looked at
 * before the gate runs, and deleted once the last gate that reads its reports
 * has run (see `cleanupPoints`), or on the way out if the run is abandoned
 * first, but only if the gate got as far as running cargo and the directory is
 * this run's. The results ride on the owning gate's outcome as `cleanups`; they
 * are not outcomes of their own, so gate counts are unchanged.
 *
 * @typedef {{ ok: boolean, detail: string, ms: number, exitCode?: number | null }} GateResult
 * @param {Gate[]} gates
 * @param {{ run?: (gate: Gate) => GateResult,
 *           platform?: NodeJS.Platform, log?: (line: string) => void,
 *           root?: string, scratch?: ScratchContext }} [options]
 */
export function runGates(gates, options = {}) {
  const { run = runGate, platform = process.platform, log = console.log, root, scratch } = options;
  /** @type {Array<{ gate: Gate, status: string, detail: string, ms: number, cleanups?: CleanupResult[] }>} */
  const outcomes = [];
  /** @type {Map<number, number>} */
  const points = root !== undefined && scratch !== undefined ? cleanupPoints(gates) : new Map();
  /**
   * Owners that started and are not yet cleaned: what they were looking at before
   * they ran, and what is known afterwards about whether they ran cargo.
   * @type {Map<number, { outcome: (typeof outcomes)[number], snapshots: ScratchSnapshot[], evidence: OwnerEvidence }>}
   */
  const pending = new Map();

  /** @param {number} owner */
  const clean = (owner) => {
    const entry = pending.get(owner);
    if (entry === undefined || root === undefined || scratch === undefined) return;
    pending.delete(owner);
    const results = entry.snapshots.flatMap((snapshot) => cleanScratch(snapshot, root, scratch, entry.evidence));
    entry.outcome.cleanups = results;
    for (const cleanup of results) log(`  ${cleanupProblem(cleanup) ? "!" : "·"} ${describeCleanup(cleanup)}`);
  };

  try {
    gates.forEach((gate, index) => {
      const skipped = skipReason(gate, platform);
      if (skipped) {
        log(`\n— SKIP ${gate.name} (${skipped})`);
        outcomes.push({ gate, status: "skipped", detail: skipped, ms: 0 });
      } else {
        log(`\n▶ ${gate.name}\n  ${commandLine(gate)}`);
        if (root !== undefined) {
          for (const report of gate.produces ?? []) rmSync(path.join(root, report), { force: true });
        }
        const outcome = { gate, status: "running", detail: "", ms: 0 };
        outcomes.push(outcome);
        /** @type {OwnerEvidence} */
        const evidence = { ran: false, passed: false, detail: "it did not return" };
        if (points.has(index) && root !== undefined && scratch !== undefined) {
          // Registered, and looked at, before it runs: a gate that throws midway may already have written.
          const snapshots = (gate.scratch ?? []).map((entry) => snapshotScratch(entry, root, scratch));
          pending.set(index, { outcome, snapshots, evidence });
        }
        const { ok, detail, ms, exitCode } = run(gate);
        Object.assign(outcome, { status: ok ? "passed" : "failed", detail, ms });
        const started = ok || (typeof exitCode === "number" && !NOT_BUILT_EXIT_CODES.includes(exitCode));
        Object.assign(evidence, { ran: started, passed: ok, detail });
        if (!ok) log(`✗ ${gate.name} failed (${detail}) — continuing so the rest still report`);
      }
      for (const [owner, due] of points) if (due === index) clean(owner);
    });
  } finally {
    // Cut short (a thrown gate runner): whatever was left behind still goes.
    for (const owner of [...pending.keys()]) clean(owner);
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
  // Cleanup is reported apart from the gates: it is not one, and a removal that
  // did not happen must be as visible here as one that did.
  const cleanups = outcomes.flatMap((o) => (o.cleanups ?? []).map((cleanup) => ({ gate: o.gate, cleanup })));
  if (cleanups.length) {
    lines.push("", "  scratch output");
    for (const { gate, cleanup } of cleanups) lines.push(`    ${gate.name}: ${describeCleanup(cleanup)}`);
    const stuck = cleanups.filter(({ cleanup }) => cleanupProblem(cleanup));
    if (stuck.length) {
      lines.push(`  ${stuck.length} scratch director${stuck.length === 1 ? "y" : "ies"} NOT REMOVED: the disk is not as this run found it`);
    }
  }
  lines.push("");
  return lines.join("\n");
}

function usage() {
  return formatUsage({
    name: "ci-local",
    summary: "Run the local gates (a subset of CI), without one failure hiding the rest.",
    flags: [
      { flag: "--list", description: "Print the gates and exit" },
      { flag: "--help, -h", description: "Show this message" },
    ],
    environment: [
      {
        flag: KEEP_COVERAGE_BUILD_ENV,
        description:
          "Set to 1 (or true, yes) to keep the instrumented coverage build (about 50 GiB in <cargo target>/llvm-cov-target) after the run; any other value, including 0 and no, deletes it; " +
          "an incremental coverage rebuild is about 15 minutes faster. Unset, the runner deletes it once the coverage floor has read lcov.info",
      },
    ],
    exits:
      "0 every gate that ran passed · 1 at least one gate failed · " +
      `${EXIT_SCRATCH_NOT_REMOVED} every gate passed but the coverage build was not removed (not for one kept on request)`,
  });
}

/**
 * `deps` is the test seam: the gates, how one runs, the repository root, the
 * scratch context and where output goes. Left out, they are the real ones.
 *
 * @param {string[]} argv
 * @param {{ gates?: Gate[], run?: (gate: Gate) => GateResult,
 *           root?: string, scratch?: ScratchContext, log?: (line: string) => void }} [deps]
 */
export function main(argv = process.argv.slice(2), deps = {}) {
  const { gates = GATES, run, root = REPO_ROOT, scratch = productionScratchContext(), log = console.log } = deps;
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
  const outcomes = runGates(gates, { root, scratch, run, log });
  log(summarize(outcomes));
  if (outcomes.some((o) => o.status === "failed")) return 1;
  // The gates were green but the disk is not as it was: not a pass either.
  const stuck = outcomes.some((o) => (o.cleanups ?? []).some(cleanupProblem));
  return stuck ? EXIT_SCRATCH_NOT_REMOVED : 0;
}

if (process.argv[1]?.endsWith("ci-local.mjs")) {
  process.exitCode = main();
}
