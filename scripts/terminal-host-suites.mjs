#!/usr/bin/env node
/**
 * Runs the terminal integration suites against the unconditional Unix terminal
 * host (`gitpulse-ptyd`).
 *
 * `docs/TERMINAL_HOST.md`, migration step 3: on Unix every `tests/common`
 * `terminals()` registry runs directly against `gitpulse-ptyd` (with
 * `GITPULSE_TEST_TERMINAL_BACKEND=host` still exported for clarity). This
 * runs the suites with CI's flags (`--locked --no-fail-fast`, one thread).
 *
 * The host is Unix-only. On Windows this exits 2, "could not run", rather than
 * passing a check that never ran.
 *
 * Exit codes: cargo's own · 2 this platform has no terminal host.
 */
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** The suites both backends must pass. */
export const SUITES = Object.freeze([
  "terminal_pty_stress",
  "terminal_session_lifecycle",
  "terminal_session_survival",
  "terminal_hardening",
  "terminal_incident",
  "terminal_output_emits",
  "terminal_ipc_latency",
]);

export function cargoArgs(suites = SUITES) {
  return [
    "test",
    "--manifest-path",
    "src-tauri/Cargo.toml",
    "--locked",
    "--no-fail-fast",
    ...suites.flatMap((suite) => ["--test", suite]),
    "--",
    "--nocapture",
    "--test-threads=1",
  ];
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.platform === "win32") {
    console.error("terminal-host-suites: Windows has no terminal host; nothing was run.");
    process.exit(2);
  }
  const run = spawnSync("cargo", cargoArgs(), {
    cwd: REPO_ROOT,
    stdio: "inherit",
    env: { ...process.env, GITPULSE_TEST_TERMINAL_BACKEND: "host" },
  });
  if (run.error) {
    console.error(`terminal-host-suites: cargo could not start: ${run.error.message}`);
    process.exit(2);
  }
  process.exit(run.status ?? 1);
}
