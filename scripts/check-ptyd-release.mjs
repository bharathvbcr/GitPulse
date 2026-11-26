#!/usr/bin/env node
/**
 * Release-link `waylode-ptyd` the way the app bundle builds it, then check
 * that the terminal host carries none of the app's window runtime.
 *
 * Why it exists: the host shares `waylode_lib` with the GUI. Release LTO
 * (`lto = "thin"`, `codegen-units = 1`) once folded the engine's
 * `dyn SessionObserver` calls into the GUI's one implementor,
 * `workbench::terminal_run::RunObserver<Wry>`, inside the copy the host links.
 * The host's own LTO had already dropped the window stack that code needs, so
 * `waylode-ptyd` failed to link with 199 undefined `tao`/`wry`/AppKit
 * symbols, while every debug build, test and `ci:local` gate passed. Only a
 * release link of this binary shows it; `terminal::NoObserver` is the fix.
 *
 * Passing means both: the release link succeeds, and `nm` finds no symbol of
 * the app runtime (`RunObserver`, `tauri_runtime_wry`) in the binary. A host
 * that links today but carries those symbols is one LTO decision away from
 * the same failure.
 *
 * Exit codes: 0 passed · 1 the link failed or the binary carries app-runtime
 * symbols · 2 the check could not run (no frontend build, no toolchain, or a
 * child that outlived its timeout)
 */
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const MANIFEST = path.join(REPO_ROOT, "src-tauri", "Cargo.toml");
const DIST = path.join(REPO_ROOT, "dist");

/** A cold release build is minutes; bounded so a wedged toolchain reads as "could not run". */
export const BUILD_TIMEOUT_MS = 60 * 60 * 1000;
export const NM_TIMEOUT_MS = 2 * 60 * 1000;

/** Symbols that belong to the GUI's window runtime and must never be in the host. */
export const FORBIDDEN = [/RunObserver/, /tauri_runtime_wry/];

/**
 * Resolve the default release binary path, respecting `CARGO_TARGET_DIR`.
 *
 * @param {Record<string, string | undefined>} [env]
 * @returns {string}
 */
export function defaultBinaryPath(env = process.env) {
  const targetDir = env.CARGO_TARGET_DIR
    ? path.resolve(REPO_ROOT, env.CARGO_TARGET_DIR)
    : path.join(REPO_ROOT, "src-tauri", "target");
  return path.join(targetDir, "release", "waylode-ptyd");
}

/**
 * Parse CLI arguments for `check-ptyd-release.mjs`.
 *
 * @param {string[]} argv
 * @returns {{ ok: true, help: boolean, binary: string | null } | { ok: false, error: string }}
 */
export function parseArgs(argv) {
  if (wantsHelp(argv)) return { ok: true, help: true, binary: null };
  /** @type {string | null} */
  let binary = null;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--binary") {
      const next = argv[i + 1];
      if (!next || next.startsWith("-")) {
        return { ok: false, error: "--binary requires a path to an already-built release waylode-ptyd" };
      }
      binary = path.resolve(next);
      i += 1;
    } else {
      return { ok: false, error: `Unknown argument: ${arg}` };
    }
  }
  return { ok: true, help: false, binary };
}

/**
 * Non-header symbol lines in `nm` output (ignores blank lines and fat-binary
 * architecture headers such as `/path/bin (for architecture x86_64):`).
 *
 * @param {string} nmOutput
 * @returns {string[]}
 */
export function symbolLines(nmOutput) {
  return nmOutput
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line !== "" && !line.endsWith(":"));
}

/**
 * Names of the forbidden symbols in `nm` output, one per offending line.
 *
 * @param {string} nmOutput
 * @returns {string[]}
 */
export function forbiddenSymbols(nmOutput) {
  return symbolLines(nmOutput)
    .filter((line) => FORBIDDEN.some((re) => re.test(line)))
    .map((line) => line.split(/\s+/).pop() ?? line);
}

/**
 * Inspect an already-built `waylode-ptyd` binary with `nm` and verify no
 * app-runtime symbols leaked into it. Fails closed (exit 2) if `nm` fails or
 * lists zero symbols (for example, a fully stripped binary where symbol
 * inspection cannot prove absence).
 *
 * @param {string} binaryPath
 * @param {(binary: string) => { status: number | null, stdout: string, stderr: string, error?: Error }} [runNm]
 * @returns {{ code: number, message: string, offenders: string[] }}
 */
export function inspectBinary(binaryPath, runNm) {
  if (!existsSync(binaryPath)) {
    return {
      code: 2,
      message: `check-ptyd-release: could not run: binary not found at ${binaryPath}`,
      offenders: [],
    };
  }
  const nmArgs = process.platform === "darwin" ? ["-arch", "all", binaryPath] : [binaryPath];
  const nm = runNm
    ? runNm(binaryPath)
    : spawnSync("nm", nmArgs, { encoding: "utf8", timeout: NM_TIMEOUT_MS, maxBuffer: 256 * 1024 * 1024 });
  if (nm.error || nm.status !== 0) {
    return {
      code: 2,
      message: `check-ptyd-release: could not run: nm ${binaryPath}: ${nm.error?.message ?? (nm.stderr ?? "").trim()}`,
      offenders: [],
    };
  }
  const stdout = nm.stdout ?? "";
  if (symbolLines(stdout).length === 0) {
    return {
      code: 2,
      message: `check-ptyd-release: could not run: nm listed zero symbols for ${binaryPath} (binary may be stripped)`,
      offenders: [],
    };
  }
  const offenders = forbiddenSymbols(stdout);
  if (offenders.length > 0) {
    return {
      code: 1,
      message: `FAIL: waylode-ptyd carries ${offenders.length} app-runtime symbol(s), e.g. ${offenders.slice(0, 3).join(", ")}`,
      offenders,
    };
  }
  return {
    code: 0,
    message: `OK: ${path.basename(binaryPath)} links in release and carries no app-runtime symbols`,
    offenders: [],
  };
}

/** @param {string[]} [argv] */
export function main(argv = process.argv.slice(2)) {
  const parsed = parseArgs(argv);
  if (!parsed.ok) {
    console.error(`check-ptyd-release: ${parsed.error}`);
    return 2;
  }
  if (parsed.help) {
    console.log(formatUsage({
      name: "check-ptyd-release",
      summary: "Verify release waylode-ptyd links cleanly and carries no app-runtime symbols.",
      flags: [
        { flag: "--binary <path>", description: "inspect an already-built release waylode-ptyd binary without rebuilding" },
      ],
      exits: "0 clean; 1 link failed or forbidden symbols present; 2 check could not run",
    }));
    return 0;
  }

  const binaryPath = parsed.binary ?? defaultBinaryPath();
  if (!parsed.binary) {
    if (!existsSync(DIST)) {
      console.error("check-ptyd-release: could not run: dist/ is missing; `bun run build` first (the release build embeds it)");
      return 2;
    }
    // The bundle's own cargo invocation (scripts/tauri.mjs -> tauri build), narrowed to this binary.
    const build = spawnSync(
      "cargo",
      ["build", "--manifest-path", MANIFEST, "--release", "--locked", "--features", "tauri/custom-protocol", "--bin", "waylode-ptyd"],
      { stdio: ["ignore", "inherit", "pipe"], encoding: "utf8", timeout: BUILD_TIMEOUT_MS, maxBuffer: 256 * 1024 * 1024 },
    );
    if (build.error) {
      console.error(`check-ptyd-release: could not run: cargo: ${build.error.message}`);
      return 2;
    }
    process.stderr.write(build.stderr ?? "");
    if (build.status !== 0) {
      const undefinedSymbols = /Undefined symbols/.test(build.stderr ?? "");
      console.error(
        undefinedSymbols
          ? "FAIL: waylode-ptyd does not link in release: undefined symbols (see above)"
          : `FAIL: the release build of waylode-ptyd exited ${build.status}`,
      );
      return 1;
    }
  }

  const verdict = inspectBinary(binaryPath);
  if (verdict.code === 0) console.log(verdict.message);
  else console.error(verdict.message);
  return verdict.code;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main());
}
