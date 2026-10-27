#!/usr/bin/env node
/**
 * Run go/cmd/gusset-check against DevCouncil's umbrella archive.
 *
 * go/go.mod replaces DevCouncil and gusset with ../../DevCouncil and
 * ../../gusset, so both must be checked out next to this repository. A linked
 * worktree under `.claude/worktrees/<name>` sits deeper than that, so the
 * checkouts are found the way the vendoring script finds them (environment,
 * then ancestors, then the canonical checkout's siblings) and, when they are
 * not where go.mod points, handed to go through a temporary go.work whose
 * replaces take precedence over the module's.
 * DevCouncil's rust/gusset-engine/cgo-env.sh builds the archive and prints
 * the cgo environment, including the archive-hash key without which Go
 * relinks a stale archive from its cache.
 *
 * Exit codes: 0 the engine passed · 1 it failed · 2 the check could not run
 * (including a child that outlived its timeout)
 */
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { findSibling, refuseBadGitBound } from "./vendor-crates.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const GO_DIR = path.join(REPO_ROOT, "go");
const DEVCOUNCIL_MODULE = "github.com/bharathvbcr/DevCouncil/backend/go_orchestrator";
const GUSSET_MODULE = "github.com/bharathvbcr/gusset";

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

/**
 * The go.work that points go/ at the DevCouncil and gusset checkouts actually
 * found, or null when they are exactly where go.mod's relative replaces
 * already point (a plain sibling checkout, and CI), so nothing is overridden.
 *
 * @param {{ goDir: string, devcouncil: string, gusset: string, goVersion: string }} roots
 * @returns {string | null}
 */
export function workspaceFor({ goDir, devcouncil, gusset, goVersion }) {
  const defaults = {
    devcouncil: path.resolve(goDir, "..", "..", "DevCouncil"),
    gusset: path.resolve(goDir, "..", "..", "gusset"),
  };
  if (path.resolve(devcouncil) === defaults.devcouncil && path.resolve(gusset) === defaults.gusset) return null;
  return [
    `go ${goVersion}`,
    "",
    `use ${JSON.stringify(goDir)}`,
    "",
    `replace ${DEVCOUNCIL_MODULE} => ${JSON.stringify(path.join(devcouncil, "backend", "go_orchestrator"))}`,
    `replace ${GUSSET_MODULE} => ${JSON.stringify(gusset)}`,
    "",
  ].join("\n");
}

/**
 * @param {string} goMod
 * @returns {string}
 */
export function goVersionOf(goMod) {
  const match = /^go\s+(\S+)\s*$/m.exec(goMod);
  if (!match) throw new Error("go/go.mod declares no go version");
  return match[1];
}

function main() {
  const refused = refuseBadGitBound();
  if (refused !== null) return refused;
  const devcouncil = process.env.GITPULSE_DEVCOUNCIL_ROOT ?? findSibling("DevCouncil", REPO_ROOT);
  const gusset = process.env.GITPULSE_GUSSET_ROOT ?? findSibling("gusset", REPO_ROOT);
  const envScript = path.join(devcouncil, "rust", "gusset-engine", "cgo-env.sh");
  for (const [sibling, variable] of [
    [envScript, "GITPULSE_DEVCOUNCIL_ROOT"],
    [path.join(gusset, "go.mod"), "GITPULSE_GUSSET_ROOT"],
  ]) {
    if (!existsSync(sibling)) {
      console.error(
        `gusset-check: ${sibling} is missing; check out DevCouncil and gusset next to this repository or set ${variable}`,
      );
      return 2;
    }
  }
  let workspace;
  try {
    workspace = workspaceFor({
      goDir: GO_DIR,
      devcouncil,
      gusset,
      goVersion: goVersionOf(readFileSync(path.join(GO_DIR, "go.mod"), "utf8")),
    });
  } catch (error) {
    console.error(`gusset-check: ${error instanceof Error ? error.message : String(error)}`);
    return 2;
  }
  const scratch = workspace === null ? null : mkdtempSync(path.join(os.tmpdir(), "gitpulse-gusset-"));
  try {
    /** @type {Record<string, string>} */
    const goWork = {};
    if (scratch !== null && workspace !== null) {
      goWork.GOWORK = path.join(scratch, "go.work");
      writeFileSync(goWork.GOWORK, workspace);
    }
    return run(envScript, goWork);
  } finally {
    if (scratch !== null) rmSync(scratch, { recursive: true, force: true });
  }
}

/**
 * @param {string} envScript
 * @param {Record<string, string>} goWork
 */
function run(envScript, goWork) {
  const envResult = spawnBounded(
    "bash",
    [envScript],
    { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] },
    ENV_SCRIPT_TIMEOUT_MS,
  );
  if (envResult.error || envResult.status !== 0) {
    console.error(`gusset-check: cgo-env.sh failed: ${envResult.error?.message ?? `exit ${envResult.status}`}`);
    return 2;
  }
  const cgoEnv = parseEnvLines(String(envResult.stdout));
  if (cgoEnv.CGO_ENABLED !== "1" || !cgoEnv.CGO_CFLAGS) {
    console.error("gusset-check: cgo-env.sh printed no cgo environment");
    return 2;
  }
  const result = spawnBounded(
    "go",
    ["run", "-tags", "gusset", "./cmd/gusset-check"],
    { cwd: GO_DIR, env: { ...process.env, ...cgoEnv, ...goWork }, stdio: "inherit" },
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
