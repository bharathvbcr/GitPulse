#!/usr/bin/env node
/**
 * Run go/cmd/gusset-check against DevCouncil's umbrella archive.
 *
 * go/go.mod replaces DevCouncil and gusset with ../../DevCouncil and
 * ../../gusset, so both must be checked out next to this repository.
 * DevCouncil's rust/gusset-engine/cgo-env.sh builds the archive and prints
 * the cgo environment, including the archive-hash key without which Go
 * relinks a stale archive from its cache.
 *
 * Exit codes: 0 the engine passed · 1 it failed · 2 the check could not run
 * (including a child that outlived its timeout)
 */
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const GO_DIR = path.join(REPO_ROOT, "go");
const ENV_SCRIPT = path.resolve(REPO_ROOT, "..", "DevCouncil", "rust", "gusset-engine", "cgo-env.sh");

// cgo-env.sh may build the engine archive cold with cargo; `go run` compiles
// the cgo program. Both are bounded so a wedged toolchain cannot hold a CI leg
// until the job timeout, and a timeout reads as "could not run", never "passed".
export const ENV_SCRIPT_TIMEOUT_MS = 20 * 60 * 1000;
export const CHECK_TIMEOUT_MS = 10 * 60 * 1000;

/**
 * spawnSync with an explicit deadline. A child killed at the deadline surfaces
 * as `result.error` (code ETIMEDOUT), which callers already map to exit 2.
 *
 * @param {string} program
 * @param {string[]} args
 * @param {import("node:child_process").SpawnSyncOptions} options
 * @param {number} timeoutMs
 */
export function spawnBounded(program, args, options, timeoutMs) {
  return spawnSync(program, args, { ...options, timeout: timeoutMs, killSignal: "SIGKILL" });
}

/**
 * Parse cgo-env.sh's KEY=VALUE lines.
 *
 * @param {string} text
 * @returns {Record<string, string>}
 */
export function parseEnvLines(text) {
  /** @type {Record<string, string>} */
  const env = {};
  for (const line of text.split("\n")) {
    const at = line.indexOf("=");
    if (at > 0) env[line.slice(0, at)] = line.slice(at + 1);
  }
  return env;
}

function main() {
  for (const sibling of [ENV_SCRIPT, path.resolve(REPO_ROOT, "..", "gusset", "go.mod")]) {
    if (!existsSync(sibling)) {
      console.error(`gusset-check: ${sibling} is missing; check out DevCouncil and gusset next to this repository`);
      return 2;
    }
  }
  const envResult = spawnBounded(
    "bash",
    [ENV_SCRIPT],
    { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] },
    ENV_SCRIPT_TIMEOUT_MS,
  );
  if (envResult.error || envResult.status !== 0) {
    console.error(`gusset-check: cgo-env.sh failed: ${envResult.error?.message ?? `exit ${envResult.status}`}`);
    return 2;
  }
  const cgoEnv = parseEnvLines(envResult.stdout);
  if (cgoEnv.CGO_ENABLED !== "1" || !cgoEnv.CGO_CFLAGS) {
    console.error("gusset-check: cgo-env.sh printed no cgo environment");
    return 2;
  }
  const result = spawnBounded(
    "go",
    ["run", "-tags", "gusset", "./cmd/gusset-check"],
    { cwd: GO_DIR, env: { ...process.env, ...cgoEnv }, stdio: "inherit" },
    CHECK_TIMEOUT_MS,
  );
  if (result.error) {
    console.error(`gusset-check: go could not run: ${result.error.message}`);
    return 2;
  }
  return result.status ?? 1;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  process.exit(main());
}
