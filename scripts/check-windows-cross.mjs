#!/usr/bin/env node
/**
 * Local Windows MSVC cross-check for GitPulse from macOS or Linux via `cargo-xwin`.
 *
 * Catches Windows-only compile errors, `#[cfg(windows)]` drift, clippy warnings,
 * and `tests/windows_runtime.rs` / cross-platform test compilation regressions
 * locally before pushing to GitHub Actions Windows runners.
 *
 * Verifies every tool required by `cargo-xwin`, C crate builds (`rusqlite`,
 * `tree-sitter`), `src-tauri/build.rs` (`tests.manifest`), and `tauri-winres` /
 * `embed-resource` (`clang-cl`, `lld-link`, `llvm-lib`, and `llvm-rc` or `$RC`).
 *
 * Isolates build artifacts in `.gitpulse/scratch/target-win-cross` by default
 * so host target caches are never disturbed. Fails closed with exit code 2 and
 * exact install steps when prerequisites or `dist/index.html` are absent.
 *
 * Exit codes: 0 passed · 1 Windows compile/clippy failed · 2 prerequisites
 * missing or invalid arguments.
 */

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

export const WINDOWS_TARGETS = Object.freeze({
  x64: "x86_64-pc-windows-msvc",
  arm64: "aarch64-pc-windows-msvc",
});

export const HOMEBREW_LLVM_BIN_DIRS = Object.freeze([
  "/opt/homebrew/opt/llvm/bin",
  "/usr/local/opt/llvm/bin",
]);

export const HOMEBREW_LLD_BIN_DIRS = Object.freeze([
  "/opt/homebrew/opt/lld/bin",
  "/usr/local/opt/lld/bin",
]);

const LLVM_CORE_TOOLS = Object.freeze(["clang-cl", "llvm-lib", "llvm-rc"]);

/**
 * Resolves the child environment for probing and running `cargo-xwin`.
 *
 * On macOS, Homebrew installs `llvm` keg-only (`/opt/homebrew/opt/llvm/bin` on
 * Apple Silicon, `/usr/local/opt/llvm/bin` on Intel), and LLVM 23+ splits `lld`
 * into its own formula (`/opt/homebrew/opt/lld/bin` / `/usr/local/opt/lld/bin`).
 * When required LLVM MSVC cross tools are absent from `PATH`, prepend any
 * installed Homebrew `llvm` / `lld` `bin` dirs so `check:windows:cross` works
 * even under minimal/non-login `PATH`s without editing shell rc.
 *
 * @param {Record<string, string | undefined>} [env]
 * @param {NodeJS.Platform} [platform]
 * @param {(p: string) => boolean} [pathExists]
 * @returns {{
 *   env: Record<string, string | undefined>,
 *   addedLlvmBin: string | null,
 *   addedLldBin: string | null,
 *   addedBins: string[],
 * }}
 */
export function resolveCrossEnv(
  env = process.env,
  platform = process.platform,
  pathExists = existsSync,
) {
  if (platform !== "darwin") {
    return { env, addedLlvmBin: null, addedLldBin: null, addedBins: [] };
  }
  const currentPath = env.PATH ?? "";
  const pathEntries = currentPath.split(path.delimiter).filter(Boolean);
  /**
   * @param {string[]} dirs
   * @param {string} tool
   */
  const hasTool = (dirs, tool) => dirs.some((dir) => pathExists(path.join(dir, tool)));

  /** @type {string | null} */
  let addedLlvmBin = null;
  /** @type {string | null} */
  let addedLldBin = null;
  /** @type {string[]} */
  const addedBins = [];

  const needsCoreLlvm = LLVM_CORE_TOOLS.some((tool) => !hasTool(pathEntries, tool));
  if (needsCoreLlvm) {
    for (const dir of HOMEBREW_LLVM_BIN_DIRS) {
      if (pathEntries.includes(dir)) continue;
      if (pathExists(path.join(dir, "clang-cl"))) {
        addedLlvmBin = dir;
        addedBins.push(dir);
        break;
      }
    }
  }

  const effectiveEntries = [...addedBins, ...pathEntries];
  if (!hasTool(effectiveEntries, "lld-link")) {
    for (const dir of HOMEBREW_LLD_BIN_DIRS) {
      if (effectiveEntries.includes(dir)) continue;
      if (pathExists(path.join(dir, "lld-link"))) {
        addedLldBin = dir;
        addedBins.push(dir);
        break;
      }
    }
  }

  if (addedBins.length === 0) {
    return { env, addedLlvmBin: null, addedLldBin: null, addedBins: [] };
  }
  const prefix = addedBins.join(path.delimiter);
  const nextPath = currentPath ? `${prefix}${path.delimiter}${currentPath}` : prefix;
  return {
    env: { ...env, PATH: nextPath },
    addedLlvmBin,
    addedLldBin,
    addedBins,
  };
}

/**
 * @typedef {{
 *   arch: "x64" | "arm64",
 *   mode: "check" | "clippy" | "tests",
 *   targetDir: string,
 *   xwinCacheDir: string,
 *   help: boolean,
 * }} WinCrossOptions
 */

/**
 * @typedef {{
 *   status: number | null,
 *   stdout: string,
 *   error?: Error,
 * }} ProbeResult
 */

/**
 * @param {string[]} argv
 * @param {string} [repoRoot]
 * @returns {WinCrossOptions}
 */
export function parseArgs(argv, repoRoot = REPO_ROOT) {
  /** @type {WinCrossOptions} */
  const opts = {
    arch: "x64",
    mode: "check",
    targetDir: path.join(repoRoot, ".gitpulse", "scratch", "target-win-cross"),
    xwinCacheDir: process.env.XWIN_CACHE_DIR || path.join(homedir(), ".cache", "gitpulse-xwin"),
    help: wantsHelp(argv),
  };
  if (opts.help) return opts;
  /** @type {string | null} */
  let modeFlag = null;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--clippy" || arg === "--tests" || arg === "--check") {
      if (modeFlag !== null && modeFlag !== arg) {
        throw new Error(`Conflicting mode flags: ${modeFlag} and ${arg} (use at most one of --check, --clippy, --tests)`);
      }
      modeFlag = arg;
      opts.mode = arg === "--clippy" ? "clippy" : arg === "--tests" ? "tests" : "check";
      continue;
    }
    const next = () => {
      const value = argv[++i];
      if (value === undefined || value.length === 0 || value.startsWith("--") || value === "-h") {
        throw new Error(`${arg} requires a value`);
      }
      return value;
    };
    if (arg === "--arch") {
      const value = next();
      if (value !== "x64" && value !== "arm64") {
        throw new Error(`Invalid --arch ${JSON.stringify(value)}; expected x64 or arm64`);
      }
      opts.arch = value;
    } else if (arg === "--target-dir") {
      opts.targetDir = path.resolve(next());
    } else if (arg === "--xwin-cache-dir") {
      opts.xwinCacheDir = path.resolve(next());
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }
  return opts;
}

export function usage() {
  return formatUsage({
    name: "check-windows-cross",
    summary: "Cross-check GitPulse's Rust targets (including tests and examples) against Windows MSVC via cargo-xwin.",
    flags: [
      { flag: "--arch <x64|arm64>", description: "Windows MSVC target architecture (default: x64)" },
      { flag: "--check", description: "Run `cargo xwin check --all-targets` (default)" },
      { flag: "--clippy", description: "Run `cargo xwin clippy --all-targets -- -D warnings`" },
      { flag: "--tests", description: "Compile Rust test binaries for Windows (`cargo xwin test --no-run`)" },
      { flag: "--target-dir <dir>", description: "Scratch Cargo target dir (default: .gitpulse/scratch/target-win-cross)" },
      { flag: "--xwin-cache-dir <dir>", description: "Cached Windows SDK / CRT directory" },
      { flag: "--help, -h", description: "Show this message" },
    ],
    environment: [
      { flag: "XWIN_CACHE_DIR", description: "Default directory for cached Microsoft CRT / Windows SDK files" },
      { flag: "RC_<target> / RC", description: "Override Windows resource compiler used by tauri-winres (default: llvm-rc)" },
    ],
    exits: "0 Windows cross-check passed · 1 compile/clippy failed · 2 missing cargo-xwin/LLVM/target or bad args",
  });
}

/**
 * Resolves the Windows resource compiler `embed-resource` (`tauri-winres`) will
 * invoke on non-Windows hosts targeting `*-pc-windows-msvc`.
 * Matches `embed-resource-3.0.12/src/lib.rs:env_target_and_rc`.
 *
 * @param {string} target
 * @param {Record<string, string | undefined>} [env]
 * @returns {{ program: string, envVar: string | null }}
 */
export function resolveRcCompiler(target, env = process.env) {
  const exactKey = `RC_${target}`;
  if (env[exactKey]) return { program: env[exactKey], envVar: exactKey };
  const underscoredKey = `RC_${target.replace(/-/g, "_")}`;
  if (env[underscoredKey]) return { program: env[underscoredKey], envVar: underscoredKey };
  if (env.RC) return { program: env.RC, envVar: "RC" };
  return { program: "llvm-rc", envVar: null };
}

/**
 * Builds the `cargo xwin ...` argument vector.
 * @param {WinCrossOptions} opts
 * @param {string} [repoRoot]
 */
export function buildXwinArgs(opts, repoRoot = REPO_ROOT) {
  const manifest = path.join(repoRoot, "src-tauri", "Cargo.toml");
  const target = WINDOWS_TARGETS[opts.arch];
  const base = [
    "xwin",
    opts.mode === "clippy" ? "clippy" : opts.mode === "tests" ? "test" : "check",
    "--manifest-path",
    manifest,
    "--locked",
    "--target",
    target,
    "--target-dir",
    opts.targetDir,
  ];
  if (opts.mode === "tests") {
    base.push("--no-run");
  } else {
    base.push("--all-targets");
  }
  if (opts.mode === "clippy") {
    base.push("--", "-D", "warnings");
  }
  return base;
}

/**
 * Probes whether an executable runs successfully with at least one of `flagSets`.
 * Stops immediately if spawning fails (e.g. ENOENT).
 *
 * @param {(program: string, args: string[]) => ProbeResult} probe
 * @param {string} program
 * @param {string[][]} flagSets
 * @returns {boolean}
 */
function probeAny(probe, program, flagSets) {
  for (const args of flagSets) {
    const res = probe(program, args);
    if (!res.error && res.status === 0) return true;
    if (res.error) return false;
  }
  return false;
}

/**
 * Checks prerequisites using injectable command runner (for unit tests).
 *
 * Unlike Xcode Command Line Tools' `/usr/bin/clang`, `cargo-xwin` + `tauri-winres`
 * (`embed-resource` `.manifest_required()`) + `src-tauri/build.rs` require LLVM's
 * MSVC-compatible tools (`clang-cl`, `lld-link`, `llvm-lib`, and `llvm-rc` or `$RC`).
 *
 * @param {WinCrossOptions} opts
 * @param {(program: string, args: string[]) => ProbeResult} [probe]
 * @param {Record<string, string | undefined>} [env]
 * @returns {{ ok: true } | { ok: false, reasons: string[] }}
 */
export function checkPrerequisites(opts, probe = defaultProbe, env = process.env) {
  /** @type {string[]} */
  const reasons = [];
  const target = WINDOWS_TARGETS[opts.arch];
  const targets = probe("rustup", ["target", "list", "--installed"]);
  if (targets.error || targets.status !== 0) {
    reasons.push("`rustup` could not be queried; install rustup and add the target with `rustup target add " + target + "`");
  } else {
    const installed = targets.stdout.split(/\r?\n/).map((line) => line.trim());
    if (!installed.includes(target)) {
      reasons.push(`Rust target ${target} is not installed. Run: rustup target add ${target}`);
    }
  }
  const xwin = probe("cargo-xwin", ["--version"]);
  if (xwin.error || xwin.status !== 0) {
    reasons.push("`cargo-xwin` is not installed. Run: cargo install --locked cargo-xwin");
  }

  /** @type {string[]} */
  const missingLlvm = [];
  if (!probeAny(probe, "clang-cl", [["--version"]])) missingLlvm.push("clang-cl");
  if (!probeAny(probe, "lld-link", [["--version"]])) missingLlvm.push("lld-link");
  if (!probeAny(probe, "llvm-lib", [["/?"], ["--version"]])) missingLlvm.push("llvm-lib");

  const rc = resolveRcCompiler(target, env);
  const rcOk = rc.envVar
    ? probeAny(probe, rc.program, [["/?"], ["-V"], ["--version"]])
    : probeAny(probe, rc.program, [["/?"], ["--version"]]);
  if (!rcOk) {
    if (rc.envVar) {
      reasons.push(
        `Resource compiler ${JSON.stringify(rc.program)} (from ${rc.envVar}) could not be executed; ` +
          "`tauri-winres` (`embed-resource`) requires a working `llvm-rc` or `windres` to compile Windows resources.",
      );
    } else {
      missingLlvm.push("llvm-rc");
    }
  }

  if (missingLlvm.length > 0) {
    reasons.push(
      `Missing LLVM MSVC cross tools on PATH: ${missingLlvm.join(", ")} ` +
        "(required by cargo-xwin, C crates like rusqlite/tree-sitter, and tauri-winres / tests.manifest; " +
        "Apple Xcode CLT `clang` does not ship these). " +
        "On macOS: `brew install llvm lld` (auto-discovered from `/opt/homebrew/opt/llvm/bin` + `/opt/homebrew/opt/lld/bin` or `/usr/local/opt/llvm/bin` + `/usr/local/opt/lld/bin`). " +
        "On Ubuntu/Debian: `sudo apt-get install -y clang lld llvm`.",
    );
  }
  return reasons.length === 0 ? { ok: true } : { ok: false, reasons };
}

/**
 * @param {string} program
 * @param {string[]} args
 * @param {Record<string, string | undefined>} [env]
 * @returns {ProbeResult}
 */
function defaultProbe(program, args, env = process.env) {
  const res = spawnSync(program, args, { encoding: "utf8", timeout: 10_000, env });
  return { status: res.status, stdout: String(res.stdout ?? ""), error: res.error };
}

/** @param {string[]} [argv] */
export function main(argv = process.argv.slice(2)) {
  /** @type {WinCrossOptions} */
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (error) {
    console.error(`check-windows-cross: ${error instanceof Error ? error.message : String(error)}`);
    return 2;
  }
  if (opts.help) {
    console.log(usage());
    return 0;
  }
  if (!existsSync(path.join(REPO_ROOT, "dist", "index.html"))) {
    console.error("FAIL: dist/index.html is missing (src-tauri embeds ../dist). Run `bun run build` first.");
    return 2;
  }
  if (!existsSync(path.join(REPO_ROOT, "src-tauri", "tests.manifest"))) {
    console.error("FAIL: src-tauri/tests.manifest is missing (src-tauri/build.rs embeds it for Windows MSVC targets).");
    return 2;
  }
  const { env: crossEnv, addedBins } = resolveCrossEnv(process.env);
  if (addedBins.length > 0) {
    console.log(`Auto-discovered Homebrew LLVM tools at ${addedBins.join(", ")}`);
  }
  const pre = checkPrerequisites(opts, (program, args) => defaultProbe(program, args, crossEnv), crossEnv);
  if (!pre.ok) {
    console.error("FAIL: Windows cross-check prerequisites missing:");
    for (const reason of pre.reasons) console.error(`  - ${reason}`);
    return 2;
  }
  mkdirSync(opts.targetDir, { recursive: true });
  mkdirSync(opts.xwinCacheDir, { recursive: true });
  const args = buildXwinArgs(opts);
  console.log(`Running: cargo ${args.join(" ")}`);
  const result = spawnSync("cargo", args, {
    cwd: REPO_ROOT,
    stdio: "inherit",
    env: {
      ...crossEnv,
      XWIN_CACHE_DIR: opts.xwinCacheDir,
      XWIN_ARCH: opts.arch === "arm64" ? "aarch64" : "x86_64",
    },
  });
  if (result.error) {
    console.error(`FAIL: cargo xwin failed to spawn: ${result.error.message}`);
    return 2;
  }
  if (result.signal) {
    console.error(`FAIL: Windows cross-${opts.mode} (${WINDOWS_TARGETS[opts.arch]}) terminated by signal ${result.signal}`);
    return 1;
  }
  if (result.status !== 0) {
    console.error(`FAIL: Windows cross-${opts.mode} (${WINDOWS_TARGETS[opts.arch]}) exited ${result.status}`);
    return 1;
  }
  console.log(`OK: Windows cross-${opts.mode} (${WINDOWS_TARGETS[opts.arch]}) passed`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
