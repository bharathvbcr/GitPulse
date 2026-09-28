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
 */
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const GO_DIR = path.join(REPO_ROOT, "go");
const ENV_SCRIPT = path.resolve(REPO_ROOT, "..", "DevCouncil", "rust", "gusset-engine", "cgo-env.sh");

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
  const envResult = spawnSync("bash", [ENV_SCRIPT], { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });
  if (envResult.error || envResult.status !== 0) {
    console.error(`gusset-check: cgo-env.sh failed: ${envResult.error?.message ?? `exit ${envResult.status}`}`);
    return 2;
  }
  const cgoEnv = parseEnvLines(envResult.stdout);
  if (cgoEnv.CGO_ENABLED !== "1" || !cgoEnv.CGO_CFLAGS) {
    console.error("gusset-check: cgo-env.sh printed no cgo environment");
    return 2;
  }
  const result = spawnSync("go", ["run", "-tags", "gusset", "./cmd/gusset-check"], {
    cwd: GO_DIR,
    env: { ...process.env, ...cgoEnv },
    stdio: "inherit",
  });
  if (result.error) {
    console.error(`gusset-check: go could not run: ${result.error.message}`);
    return 2;
  }
  return result.status ?? 1;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  process.exit(main());
}
