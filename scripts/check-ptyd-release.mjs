#!/usr/bin/env node
/**
 * Release-link `gitpulse-ptyd` the way the app bundle builds it, then check
 * that the terminal host carries none of the app's window runtime.
 *
 * Why it exists: the host shares `gitpulse_lib` with the GUI. Release LTO
 * (`lto = "thin"`, `codegen-units = 1`) once folded the engine's
 * `dyn SessionObserver` calls into the GUI's one implementor,
 * `workbench::terminal_run::RunObserver<Wry>`, inside the copy the host links.
 * The host's own LTO had already dropped the window stack that code needs, so
 * `gitpulse-ptyd` failed to link with 199 undefined `tao`/`wry`/AppKit
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

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const MANIFEST = path.join(REPO_ROOT, "src-tauri", "Cargo.toml");
const DIST = path.join(REPO_ROOT, "dist");
const BINARY = path.join(REPO_ROOT, "src-tauri", "target", "release", "gitpulse-ptyd");

/** A cold release build is minutes; bounded so a wedged toolchain reads as "could not run". */
export const BUILD_TIMEOUT_MS = 60 * 60 * 1000;
export const NM_TIMEOUT_MS = 2 * 60 * 1000;

/** Symbols that belong to the GUI's window runtime and must never be in the host. */
export const FORBIDDEN = [/RunObserver/, /tauri_runtime_wry/];

/**
 * Names of the forbidden symbols in `nm` output, one per offending line.
 *
 * @param {string} nmOutput
 * @returns {string[]}
 */
export function forbiddenSymbols(nmOutput) {
  return nmOutput
    .split("\n")
    .filter((line) => FORBIDDEN.some((re) => re.test(line)))
    .map((line) => line.trim().split(/\s+/).pop() ?? line.trim());
}

function main() {
  if (!existsSync(DIST)) {
    console.error("check-ptyd-release: could not run: dist/ is missing; `bun run build` first (the release build embeds it)");
    return 2;
  }
  // The bundle's own cargo invocation (scripts/tauri.mjs -> tauri build), narrowed to this binary.
  const build = spawnSync(
    "cargo",
    ["build", "--manifest-path", MANIFEST, "--release", "--locked", "--features", "tauri/custom-protocol", "--bin", "gitpulse-ptyd"],
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
        ? "FAIL: gitpulse-ptyd does not link in release: undefined symbols (see above)"
        : `FAIL: the release build of gitpulse-ptyd exited ${build.status}`,
    );
    return 1;
  }
  const nm = spawnSync("nm", [BINARY], { encoding: "utf8", timeout: NM_TIMEOUT_MS, maxBuffer: 256 * 1024 * 1024 });
  if (nm.error || nm.status !== 0) {
    console.error(`check-ptyd-release: could not run: nm ${BINARY}: ${nm.error?.message ?? nm.stderr}`);
    return 2;
  }
  const offenders = forbiddenSymbols(nm.stdout);
  if (offenders.length > 0) {
    console.error(`FAIL: gitpulse-ptyd carries ${offenders.length} app-runtime symbol(s), e.g. ${offenders.slice(0, 3).join(", ")}`);
    return 1;
  }
  console.log("OK: gitpulse-ptyd links in release and carries no app-runtime symbols");
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main());
}
