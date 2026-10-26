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
 * Three rules are read here without actionlint, because a release job holds a
 * write token: every third-party `uses:` is pinned to a commit SHA, every
 * checkout sets `persist-credentials: false`, and no step pipes a download
 * straight into a shell.
 *
 * Exit codes: 0 workflows are clean · 1 actionlint or a pinning rule found
 * problems · 2 the check could not run
 */

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const WORKFLOW_DIR = path.join(REPO_ROOT, ".github", "workflows");

// actionlint reads a handful of YAML files; a minute is generous. A kill at
// the deadline arrives as `result.error`, which `interpret` maps to exit 2.
export const ACTIONLINT_TIMEOUT_MS = 60_000;

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
 * True when `yaml` declares a workflow-level `permissions:` map (before `jobs:`).
 * Job-only permissions leave the rest of the file on GitHub's default token,
 * which is the hole `actions/missing-workflow-permissions` reports.
 *
 * @param {string} yaml
 * @returns {boolean}
 */
export function hasWorkflowLevelPermissions(yaml) {
  const jobsAt = yaml.search(/^jobs\s*:/m);
  const head = jobsAt === -1 ? yaml : yaml.slice(0, jobsAt);
  return /^permissions\s*:/m.test(head);
}

/**
 * Workflow filenames in `dir` that lack a top-level permissions block.
 *
 * @param {string} dir
 * @returns {string[]}
 */
export function workflowsMissingPermissions(dir) {
  return workflowFiles(dir).filter((name) => {
    const text = readFileSync(path.join(dir, name), "utf8");
    return !hasWorkflowLevelPermissions(text);
  });
}

/**
 * Every `uses:` ref that can be repointed under the workflow: a tag, a branch
 * or an abbreviated SHA. A local action (`./`) and a docker image have no such
 * ref. A tag is the hole: whoever controls the action's repository can move it,
 * and the next run executes that code with this job's token.
 *
 * @param {string} yaml
 * @returns {string[]}
 */
export function unpinnedActions(yaml) {
  return [...yaml.matchAll(/^\s*(?:- )?uses:\s+(\S+)/gm)]
    .map((match) => match[1])
    .filter((ref) => !ref.startsWith("./") && !ref.startsWith("docker://"))
    .filter((ref) => !/@[0-9a-f]{40}$/.test(ref));
}

/**
 * Indexes (0-based, in file order) of `actions/checkout` steps whose own
 * `with:` does not set `persist-credentials: false`. Without it the checkout
 * writes the job's token into `.git/config`, where every later step — and every
 * build script it runs — can read it.
 *
 * @param {string} yaml
 * @returns {number[]}
 */
export function checkoutsPersistingCredentials(yaml) {
  const steps = yaml.split(/\n(?=[ \t]*- )/).filter((step) => /^\s*(?:- )?uses:\s+actions\/checkout@/m.test(step));
  return steps.flatMap((step, index) => (/^\s+persist-credentials:\s*false\s*$/m.test(step) ? [] : [index]));
}

/**
 * Lines that run a download without looking at it first: `bash <(curl ...)` or
 * `curl ... | bash`. A tag-addressed script is whatever its owner last pushed
 * under that name, so the job executes unreviewed code with its own token.
 * Download to a file, check a pinned SHA-256, then run it. Comments are skipped.
 *
 * @param {string} yaml
 * @returns {string[]}
 */
export function pipedDownloads(yaml) {
  return yaml
    .split(/\r?\n/)
    .filter((line) => !/^\s*#/.test(line))
    .filter((line) => /\b(?:ba|z|da|k)?sh\s+<\(\s*(?:curl|wget)\b/.test(line) || /\b(?:curl|wget)\b[^|\n]*\|\s*(?:sudo\s+)?(?:ba|z|da|k)?sh\b/.test(line))
    .map((line) => line.trim());
}

/**
 * Every rule over every workflow in `dir`, one line per finding.
 *
 * @param {string} dir
 * @returns {string[]}
 */
export function workflowPolicyFindings(dir) {
  return workflowFiles(dir).flatMap((name) => {
    const text = readFileSync(path.join(dir, name), "utf8");
    return [
      ...pipedDownloads(text).map((line) => `${name}: runs a download without checking it (${line})`),
      ...unpinnedActions(text).map((ref) => `${name}: ${ref} is not pinned to a 40-character commit SHA`),
      ...checkoutsPersistingCredentials(text).map(
        (index) => `${name}: checkout #${index + 1} does not set persist-credentials: false`,
      ),
    ];
  });
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
  // Read before actionlint and reported whatever it says: these rules need no
  // binary, so a missing actionlint must not hide them.
  const findings = workflowPolicyFindings(WORKFLOW_DIR);
  for (const finding of findings) console.error(`FAIL: ${finding}`);
  const result = spawnSync("actionlint", ["-color"], {
    cwd: REPO_ROOT,
    stdio: "inherit",
    timeout: ACTIONLINT_TIMEOUT_MS,
    killSignal: "SIGKILL",
  });
  const { code, message } = interpret(result);
  if (code !== 0) console.error(`FAIL: ${message}`);
  // A rule that was checked and broken outranks an actionlint that could not run.
  if (findings.length) return 1;
  if (code === 0) {
    console.log(`OK: ${files.length} workflow files lint clean, every action pinned to a commit, no checkout keeps its token, no download is piped into a shell (${files.join(", ")})`);
  }
  return code;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
