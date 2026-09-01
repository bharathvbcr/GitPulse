#!/usr/bin/env node
/**
 * Lint the GitHub Actions workflow files with actionlint.
 *
 * This exists because nothing else in the repository reads the workflow YAML.
 * `ci.yml` and `coverage.yml` run on every push, but `release.yml` runs only
 * on a `v*` tag — so a syntax error there stays invisible until a release is
 * cut, which is the worst moment to discover the file does not parse. A stray
 * indent on `permissions:` did exactly that once.
 *
 * actionlint is a separate binary rather than an npm dependency. A missing
 * binary exits 2 (checker could not run) and is reported differently from
 * workflows that were checked and found faulty, which exits 1.
 *
 * Exit codes: 0 workflows are clean · 1 actionlint found problems ·
 * 2 the check could not run
 */

import { spawnSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const WORKFLOW_DIR = path.join(REPO_ROOT, ".github", "workflows");

export const INSTALL_HINT =
  "actionlint is not installed. Install it with `brew install actionlint`, or see " +
  "https://github.com/rhysd/actionlint/blob/main/docs/install.md";

/**
 * Workflow files actionlint is expected to cover, so an empty directory is
 * reported as a failure rather than silently passing with nothing checked.
 *
 * @param {string} dir
 * @returns {string[]}
 */
export function workflowFiles(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((name) => name.endsWith(".yml") || name.endsWith(".yaml"))
    .sort();
}

/**
 * @param {{ status: number | null, error?: Error }} result
 * @returns {{ code: number, message: string | null }}
 */
export function interpret(result) {
  if (result.error && /** @type {NodeJS.ErrnoException} */ (result.error).code === "ENOENT") {
    return { code: 2, message: INSTALL_HINT };
  }
  if (result.error) return { code: 2, message: `actionlint could not run: ${result.error.message}` };
  if (result.status === 0) return { code: 0, message: null };
  if (result.status === 1) return { code: 1, message: "actionlint reported problems" };
  return { code: 2, message: `actionlint exited with ${result.status}` };
}

export function main() {
  const files = workflowFiles(WORKFLOW_DIR);
  if (files.length === 0) {
    console.error(`FAIL: no workflow files found in ${WORKFLOW_DIR} — nothing was checked`);
    return 2;
  }
  const result = spawnSync("actionlint", ["-color"], { cwd: REPO_ROOT, stdio: "inherit" });
  const { code, message } = interpret(result);
  if (code === 0) {
    console.log(`OK: ${files.length} workflow files lint clean (${files.join(", ")})`);
    return 0;
  }
  console.error(`FAIL: ${message}`);
  return code;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
