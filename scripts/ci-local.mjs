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
 * Exit codes: 0 every gate that ran passed · 1 at least one gate failed.
 */

import { spawnSync } from "node:child_process";
import { lstatSync, readdirSync, realpathSync, rmSync } from "node:fs";
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
  };
}

/** Set (to anything but "", "0" or "false") to keep the coverage build for an incremental rebuild. */
export const KEEP_COVERAGE_BUILD_ENV = "CI_LOCAL_KEEP_COVERAGE_BUILD";

/** Cargo drops one of these at the root of every target directory it creates. */
const CARGO_TARGET_MARKERS = ["CACHEDIR.TAG", ".rustc_info.json"];
const COVERAGE_BUILD_NAME = "llvm-cov-target";

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
 * }} ScratchContext
 * @typedef {{
 *   kind: string, status: "removed" | "absent" | "kept" | "refused" | "failed",
 *   dir: string | null, bytes: number | null, detail: string,
 * }} CleanupResult
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
 * @param {string} manifest repository-relative Cargo.toml
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
  const context = { env: process.env, cargoMetadata, home: homedir() };
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

/**
 * The directories one `cargo llvm-cov nextest --manifest-path <manifest>` run
 * writes its instrumented build to, and why each may be deleted.
 *
 * Read from cargo-llvm-cov v0.9.1, src/cargo.rs, `Workspace::new` (the version
 * installed on this machine): when `CARGO_LLVM_COV_TARGET_DIR` is set the target
 * directory is the current directory joined with it, and the build directory is
 * the current directory joined with `CARGO_LLVM_COV_BUILD_DIR` if that is set
 * (else the target directory itself); `CARGO_LLVM_COV_BUILD_DIR` is not read
 * otherwise. When neither is set they are `<metadata target_directory>/llvm-cov-target`
 * and `<metadata build_directory>/llvm-cov-target`. Either way the build
 * directory only takes effect when cargo's `build.build-dir` is configured, which
 * is exactly when `build_directory` differs from `target_directory` in
 * `cargo metadata`. The gate passes no `--target-dir`, and runs with the
 * repository root as its current directory, so relative values resolve there.
 *
 * @param {string} root
 * @param {ScratchContext} context
 * @param {CargoDirectories} cargo
 * @returns {{ dir: string, named: boolean, why: string }[]} named: spelled out by an environment variable
 */
function coverageBuildCandidates(root, context, cargo) {
  const { env } = context;
  /** @type {{ dir: string, named: boolean, why: string }[]} */
  const found = [];
  /** @param {string} dir @param {boolean} named @param {string} why */
  const add = (dir, named, why) => {
    if (!found.some((f) => f.dir === dir)) found.push({ dir, named, why });
  };
  const target = env.CARGO_LLVM_COV_TARGET_DIR;
  const build = env.CARGO_LLVM_COV_BUILD_DIR;
  // An empty value is not one this file can interpret; deleting by a guess at it is not an option.
  if (target === "") throw new RefusedError("CARGO_LLVM_COV_TARGET_DIR is set but empty, so the directory it names cannot be identified");
  if (target !== undefined && build === "") throw new RefusedError("CARGO_LLVM_COV_BUILD_DIR is set but empty, so the directory it names cannot be identified");
  const buildConfigured = cargo.buildDirectory !== cargo.targetDirectory;
  if (target !== undefined) {
    add(path.resolve(root, target), true, "CARGO_LLVM_COV_TARGET_DIR");
    if (build !== undefined && buildConfigured) add(path.resolve(root, build), true, "CARGO_LLVM_COV_BUILD_DIR");
  } else {
    add(path.join(cargo.targetDirectory, COVERAGE_BUILD_NAME), false, "<cargo target>/llvm-cov-target");
    if (buildConfigured) add(path.join(cargo.buildDirectory, COVERAGE_BUILD_NAME), false, "<cargo build>/llvm-cov-target");
  }
  return found;
}

class RefusedError extends Error {}

/**
 * Whether `dir` may be deleted as a coverage build, or the reason it may not.
 * Checked on the real path of its parent and the lstat of the leaf, so a
 * symlink at either place cannot redirect the delete.
 *
 * @param {{ dir: string, named: boolean, why: string }} candidate
 * @param {string} root
 * @param {ScratchContext} context
 * @param {CargoDirectories} cargo
 * @returns {{ ok: true, real: string } | { ok: false, reason: string }}
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
  if (!isInside(real, realRoot)) {
    // Outside the repository only a path the environment names for this build is ours to delete.
    const targetEnv = context.env.CARGO_TARGET_DIR;
    const viaCargoTargetDir =
      !named &&
      targetEnv !== undefined &&
      targetEnv !== "" &&
      path.basename(real) === COVERAGE_BUILD_NAME &&
      realpathOrNull(path.resolve(root, targetEnv)) === realParent;
    if (!named && !viaCargoTargetDir) {
      return {
        ok: false,
        reason: "it is outside the repository and is not named by CARGO_TARGET_DIR or CARGO_LLVM_COV_TARGET_DIR",
      };
    }
  }
  if (path.basename(real) !== COVERAGE_BUILD_NAME) {
    const marked = CARGO_TARGET_MARKERS.some((marker) => lstatOrNull(path.join(real, marker)) !== null);
    if (!marked) {
      return {
        ok: false,
        reason: `it is not named ${COVERAGE_BUILD_NAME} and holds none of ${CARGO_TARGET_MARKERS.join(", ")}, so it is not recognisable as a cargo build directory`,
      };
    }
  }
  return { ok: true, real };
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
 * Delete what one `scratch` entry names, or say precisely why it was not.
 * Never throws, and never reports a removal it did not confirm: after the
 * delete the path is looked at again.
 *
 * @param {Scratch} scratch
 * @param {string} root repository root
 * @param {ScratchContext} context
 * @returns {CleanupResult[]}
 */
export function cleanScratch(scratch, root, context) {
  const kind = scratch.kind;
  /** @param {CleanupResult["status"]} status @param {string | null} dir @param {string} detail @param {number | null} [bytes] @returns {CleanupResult} */
  const result = (status, dir, detail, bytes = null) => ({ kind, status, dir, bytes, detail });
  const keep = context.env[KEEP_COVERAGE_BUILD_ENV];
  if (keep !== undefined && keep !== "" && keep !== "0" && keep.toLowerCase() !== "false") {
    return [result("kept", null, `${KEEP_COVERAGE_BUILD_ENV} is set; the coverage build is left in place`)];
  }
  if (scratch.kind !== "cargo-llvm-cov") {
    return [result("refused", null, `unknown scratch kind ${JSON.stringify(scratch.kind)}`)];
  }
  /** @type {CargoDirectories} */
  let cargo;
  /** @type {{ dir: string, named: boolean, why: string }[]} */
  let candidates;
  try {
    cargo = context.cargoMetadata(scratch.manifest);
    candidates = coverageBuildCandidates(root, context, cargo);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const status = error instanceof RefusedError ? "refused" : "failed";
    return [result(status, null, `could not identify the coverage build directory: ${message}`)];
  }
  return candidates.map((candidate) => {
    const { dir } = candidate;
    const leaf = lstatOrNull(dir);
    if (leaf === null) return result("absent", dir, "nothing to remove: the directory does not exist");
    if (leaf.isSymbolicLink()) return result("refused", dir, "it is a symlink, which is never followed");
    if (!leaf.isDirectory()) return result("refused", dir, "it is not a directory");
    const verdict = vet(candidate, root, context, cargo);
    if (!verdict.ok) return result("refused", dir, verdict.reason);
    const bytes = measureBytes(verdict.real);
    try {
      (context.remove ?? ((target) => rmSync(target, { recursive: true, force: false, maxRetries: 3 })))(verdict.real);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return result("failed", dir, `removal failed: ${message}`, bytes);
    }
    if (lstatOrNull(verdict.real) !== null) return result("failed", dir, "removal reported success but the directory is still there", bytes);
    return result("removed", dir, "removed", bytes);
  });
}

/** @param {CleanupResult} cleanup */
function describeCleanup(cleanup) {
  const where = cleanup.dir ?? cleanup.kind;
  switch (cleanup.status) {
    case "removed":
      return `removed ${where}${cleanup.bytes === null ? " (size unknown)" : `, freed ${formatBytes(cleanup.bytes)}`}`;
    case "absent":
      return `${where}: ${cleanup.detail}`;
    case "kept":
      return `kept: ${cleanup.detail}`;
    default:
      return `COVERAGE BUILD NOT REMOVED (${cleanup.status}): ${where}: ${cleanup.detail}`;
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
 * With `root` and `scratch`, a gate's `scratch` output is deleted once the last
 * gate that reads its reports has run (see `cleanupPoints`), or on the way out
 * if the run is abandoned first. The results ride on the owning gate's outcome
 * as `cleanups`; they are not outcomes of their own, so gate counts are unchanged.
 *
 * @param {Gate[]} gates
 * @param {{ run?: (gate: Gate) => { ok: boolean, detail: string, ms: number },
 *           platform?: NodeJS.Platform, log?: (line: string) => void,
 *           root?: string, scratch?: ScratchContext }} [options]
 */
export function runGates(gates, options = {}) {
  const { run = runGate, platform = process.platform, log = console.log, root, scratch } = options;
  /** @type {Array<{ gate: Gate, status: string, detail: string, ms: number, cleanups?: CleanupResult[] }>} */
  const outcomes = [];
  /** @type {Map<number, number>} */
  const points = root !== undefined && scratch !== undefined ? cleanupPoints(gates) : new Map();
  /** owner index -> its outcome, for owners that ran and are not yet cleaned */
  /** @type {Map<number, (typeof outcomes)[number]>} */
  const pending = new Map();

  /** @param {number} owner */
  const clean = (owner) => {
    const outcome = pending.get(owner);
    if (outcome === undefined || root === undefined || scratch === undefined) return;
    pending.delete(owner);
    const results = (outcome.gate.scratch ?? []).flatMap((entry) => cleanScratch(entry, root, scratch));
    outcome.cleanups = results;
    for (const cleanup of results) log(`  ${cleanup.status === "removed" || cleanup.status === "absent" || cleanup.status === "kept" ? "·" : "!"} ${describeCleanup(cleanup)}`);
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
        // Registered before it runs: a gate that throws midway may already have written.
        if (points.has(index)) pending.set(index, outcome);
        outcomes.push(outcome);
        const { ok, detail, ms } = run(gate);
        Object.assign(outcome, { status: ok ? "passed" : "failed", detail, ms });
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
          "Set to 1 to keep the instrumented coverage build (about 50 GiB in <cargo target>/llvm-cov-target) after the run; " +
          "an incremental coverage rebuild is about 15 minutes faster. Unset, the runner deletes it once the coverage floor has read lcov.info",
      },
    ],
    exits: "0 every gate that ran passed · 1 at least one gate failed",
  });
}

/**
 * `deps` is the test seam: the gates, how one runs, the repository root, the
 * scratch context and where output goes. Left out, they are the real ones.
 *
 * @param {string[]} argv
 * @param {{ gates?: Gate[], run?: (gate: Gate) => { ok: boolean, detail: string, ms: number },
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
  return outcomes.some((o) => o.status === "failed") ? 1 : 0;
}

if (process.argv[1]?.endsWith("ci-local.mjs")) {
  process.exitCode = main();
}
