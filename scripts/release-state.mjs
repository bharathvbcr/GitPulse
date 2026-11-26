#!/usr/bin/env node
/** One owner for remote release identity, CI provenance, and draft finalization. */
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, closeSync, mkdirSync, mkdtempSync, openSync, readFileSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseTag } from "./check-release-version.mjs";
import { BudgetExhausted, bundleSpecs, createBudget, declaredBinaryNames, expectedInstallerNames, inspectBundleContents, inspectReleaseAssets, supplementNames } from "./check-release-assets.mjs";
import { extractNotes } from "./release-notes.mjs";
import { formatUsage, wantsHelp } from "./usage.mjs";

/** @typedef {(program: string, args: string[], input?: string) => {status: number | null, stdout: string, failed: boolean}} Runner */
/** Every `gh` and `git` call a stage makes through `runCommand` is killed after this long. */
export const COMMAND_TIMEOUT_MS = 60_000;

/** @type {Runner} */
export function runCommand(program, args, input) {
  const result = spawnSync(program, args, {
    input, encoding: "utf8", timeout: COMMAND_TIMEOUT_MS, maxBuffer: 4 * 1024 * 1024,
    env: { ...process.env, GH_PROMPT_DISABLED: "1", GIT_TERMINAL_PROMPT: "0" },
  });
  return { status: result.status, stdout: result.stdout ?? "", failed: !!result.error || !!result.signal };
}

/** One installer download may take this long and be this large; both bound the verify job. */
export const DOWNLOAD_TIMEOUT_MS = 120_000;
export const MAX_ASSET_BYTES = 512 * 1024 * 1024;
/** Bounded transient retry count for idempotent GET asset downloads inside verify. */
export const MAX_DOWNLOAD_ATTEMPTS = 3;
/** Bounded transient retry count for supplement asset uploads inside attest. */
export const MAX_UPLOAD_ATTEMPTS = 3;
/** Bounded page count when paginating GitHub REST endpoints (100 items/page -> 1,000 items max). */
export const MAX_API_PAGES = 10;
/** Bounded delete-and-recheck passes when wiping leftover assets on a resumed draft. */
export const MAX_WIPE_PASSES = 5;

/**
 * Exact push-run job names that `prepare` and `ready` require to have completed
 * with `conclusion: "success"` before a release draft can be created or resumed.
 * Kept in lockstep with `.github/workflows/ci.yml` and `.github/workflows/coverage.yml`
 * via `scripts/ci-shards-contract.test.ts`, so a skipped shard, missing completeness
 * gate, or unrun platform check cannot slip past a green workflow-level conclusion.
 *
 * @type {Readonly<Record<string, readonly string[]>>}
 */
export const REQUIRED_WORKFLOW_JOBS = Object.freeze({
  "ci.yml": Object.freeze([
    "Frontend (ubuntu-latest)",
    "Frontend (macos-latest)",
    "Frontend (windows-latest)",
    "Rust Lint (ubuntu-latest)",
    "Rust Lint (macos-latest)",
    "Rust Lint (windows-latest)",
    "Rust Tests (Linux, shard 1/3)",
    "Rust Tests (Linux, shard 2/3)",
    "Rust Tests (Linux, shard 3/3)",
    "Rust Tests (Linux, doctests and one-off checks)",
    "Rust Tests (Windows, shard 1/4)",
    "Rust Tests (Windows, shard 2/4)",
    "Rust Tests (Windows, shard 3/4)",
    "Rust Tests (Windows, shard 4/4)",
    "Rust Tests (Windows, doctests and suite list)",
    "Rust Tests (macOS, shard 1/3)",
    "Rust Tests (macOS, shard 2/3)",
    "Rust Tests (macOS, shard 3/3)",
    "Rust Tests (macOS, doctests and one-off checks)",
    "Rust Tests (every OS shard ran)",
    "Gusset engine (Linux)",
    "No commit authored as a test fixture",
  ]),
  "coverage.yml": Object.freeze([
    "Generate Code Coverage Reports",
  ]),
});

/**
 * True when an `attest` supplement upload error looks transient (5xx, 429, or
 * dropped connection) and safe to retry after cleaning any partial upload.
 *
 * @param {string} message
 * @returns {boolean}
 */
export function isTransientUploadError(message) {
  return /:\s*(?:incomplete response|HTTP (?:429|500|502|503|504))\b/.test(message);
}

/**
 * True when a `gh api` asset download failure is permanent and must fail
 * immediately without retrying (client HTTP errors or missing CLI).
 *
 * @param {string} detail
 * @param {string | undefined} [errorCode]
 * @returns {boolean}
 */
export function isPermanentDownloadFailure(detail, errorCode) {
  if (errorCode === "ENOENT" || errorCode === "EACCES") return true;
  return /\bHTTP\s*(?:400|401|403|404|422)\b|\bNot Found\b/i.test(detail);
}

/**
 * One deadline for the whole bundle check (seven downloads plus every 7z
 * call). Each call gets min(its own cap, what is left), so the check refuses by
 * name instead of the job being killed. release.yml's verify job has
 * `timeout-minutes: 25` (1500 s), and around this check the job spends, at
 * worst:
 *   3 min  checkout, setup-node and the tool check;
 *   10 min the ten bounded `gh`/`git` calls finalize makes outside the check,
 *          each capped at COMMAND_TIMEOUT_MS (60 s) by `runCommand`: three tag
 *          checks (each a `git rev-parse` and a `git ls-remote`) and four
 *          GitHub API calls (the draft read, the read before the PATCH, the
 *          PATCH and the confirming read).
 * 10 min + 3 min + 10 min = 23 min, which leaves 2 min of margin inside 25.
 * Hashing and gunzip are CPU work of a few seconds and are not drawn from it.
 * release-workflow.test.ts counts those calls by running finalize against a
 * fake runner and re-derives this sum from the workflow file, so adding a
 * call fails it until the budget or this comment is revisited.
 */
export const BUNDLE_CHECK_BUDGET_MS = 10 * 60 * 1000;

/**
 * @typedef {{id: number, name: string, size: number, digest: string}} DraftAsset
 * @typedef {(request: {repo: string, asset: DraftAsset, dest: string, timeoutMs?: number}) => void} AssetFetcher
 */

/**
 * Streams one draft asset's bytes to `dest` through `gh api`, by asset ID:
 * `/releases/tags/{tag}` cannot see a draft, and `--include` would put the
 * response headers in front of the binary. The file descriptor is the child's
 * stdout, so the bytes never pass through a buffer here. Size and digest are
 * checked by the caller, which owns the bound on what it will open.
 *
 * Retries transient network/5xx failures up to `MAX_DOWNLOAD_ATTEMPTS` within
 * the single caller-supplied `timeoutMs` deadline, removing partial files on
 * every failed attempt. Permanent errors (404/401/403/ENOENT) fail immediately.
 *
 * @type {AssetFetcher}
 */
export function fetchReleaseAsset({repo, asset, dest, timeoutMs = DOWNLOAD_TIMEOUT_MS}) {
  const budgetCap = Math.min(timeoutMs, DOWNLOAD_TIMEOUT_MS);
  const deadline = Date.now() + budgetCap;
  /** @type {Error | null} */
  let lastError = null;
  for (let attempt = 1; attempt <= MAX_DOWNLOAD_ATTEMPTS; attempt += 1) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) {
      throw lastError ?? new Error(`Download of ${asset.name} failed: timed out`);
    }
    const fd = openSync(dest, "wx", 0o600);
    let succeeded = false;
    try {
      const result = spawnSync("gh", ["api", "-H", "Accept: application/octet-stream", `repos/${repo}/releases/assets/${asset.id}`], {
        stdio: ["ignore", fd, "pipe"], encoding: "utf8", timeout: remaining, maxBuffer: 1024 * 1024,
        env: { ...process.env, GH_PROMPT_DISABLED: "1" },
      });
      if (!result.error && !result.signal && result.status === 0) {
        succeeded = true;
        return;
      }
      const detail = result.error?.message ?? result.signal ?? (result.stderr ?? "").trim().slice(0, 200);
      const errCode = /** @type {NodeJS.ErrnoException | undefined} */ (result.error)?.code;
      lastError = new Error(`Download of ${asset.name} failed: ${detail}`);
      if (isPermanentDownloadFailure(detail, errCode) || errCode === "ETIMEDOUT") {
        throw lastError;
      }
    } finally {
      closeSync(fd);
      if (!succeeded) rmSync(dest, { force: true });
    }
  }
  throw lastError ?? new Error(`Download of ${asset.name} failed`);
}

/**
 * Validate both SBOM JSON payloads before attesting so a corrupt or empty
 * document from an upstream generator never gets uploaded to a release draft.
 *
 * @param {{ spdx: string, cargo: string } | undefined} sboms
 */
export function validateSbomDocuments(sboms) {
  if (![sboms?.spdx, sboms?.cargo].every(text => typeof text === "string" && text.length > 0 && Buffer.byteLength(text) <= 32 * 1024 * 1024)) {
    throw new Error("Missing, empty or oversized SBOM");
  }
  const spdxText = /** @type {{ spdx: string, cargo: string }} */ (sboms).spdx;
  const cargoText = /** @type {{ spdx: string, cargo: string }} */ (sboms).cargo;
  let spdxDoc;
  try {
    spdxDoc = record(JSON.parse(spdxText));
  } catch {
    throw new Error("Invalid SPDX SBOM: expected a JSON object");
  }
  if (typeof spdxDoc.spdxVersion !== "string" || spdxDoc.spdxVersion.trim() === "") {
    throw new Error("Invalid SPDX SBOM: missing non-empty spdxVersion");
  }
  if (spdxDoc.packages !== undefined && !Array.isArray(spdxDoc.packages)) {
    throw new Error("Invalid SPDX SBOM: packages must be an array");
  }
  let cargoDoc;
  try {
    cargoDoc = record(JSON.parse(cargoText));
  } catch {
    throw new Error("Invalid CycloneDX SBOM: expected a JSON object");
  }
  if (cargoDoc.bomFormat !== "CycloneDX") {
    throw new Error(`Invalid CycloneDX SBOM: expected bomFormat "CycloneDX", got ${JSON.stringify(cargoDoc.bomFormat)}`);
  }
  if (cargoDoc.components !== undefined && !Array.isArray(cargoDoc.components)) {
    throw new Error("Invalid CycloneDX SBOM: components must be an array");
  }
}

/**
 * Deterministic UTF-16/ASCII code-point comparator, independent of host ICU/OS locale.
 *
 * @param {string} a
 * @param {string} b
 * @returns {number}
 */
export function compareCodePoints(a, b) {
  return a < b ? -1 : a > b ? 1 : 0;
}

/**
 * Formats a deterministic, locale-independent `sha256sum -c` manifest sorted
 * strictly by code-point order with LF line endings.
 *
 * @param {Array<{ name: string, digest: string }>} entries
 * @returns {string}
 */
export function formatChecksumsManifest(entries) {
  if (!Array.isArray(entries) || entries.length === 0) {
    throw new Error("Checksums manifest requires at least one entry");
  }
  /** @type {Array<{ name: string, hex: string }>} */
  const normalized = [];
  const seen = new Set();
  for (const entry of entries) {
    if (!entry || typeof entry.name !== "string" || entry.name === "" || /[\s\r\n]/.test(entry.name)) {
      throw new Error(`Invalid checksums manifest filename: ${JSON.stringify(entry?.name)}`);
    }
    if (seen.has(entry.name)) {
      throw new Error(`Duplicate checksums manifest entry: ${entry.name}`);
    }
    seen.add(entry.name);
    const raw = typeof entry.digest === "string" ? entry.digest : "";
    const hex = raw.startsWith("sha256:") ? raw.slice("sha256:".length) : raw;
    if (!/^[a-f0-9]{64}$/i.test(hex)) {
      throw new Error(`Invalid SHA-256 digest for ${entry.name}`);
    }
    normalized.push({ name: entry.name, hex: hex.toLowerCase() });
  }
  normalized.sort((a, b) => compareCodePoints(a.name, b.name));
  return normalized.map(({ hex, name }) => `${hex}  ${name}\n`).join("");
}

/**
 * Strictly parses a `sha256sum -c` manifest (`<64 lowercase hex>  <filename>\n`).
 * Refuses CRLF, missing trailing newline, blank lines, uppercase hex, malformed
 * entries, or duplicate filenames.
 *
 * @param {string} text
 * @returns {Map<string, string>} map of filename -> `"sha256:<hex>"`
 */
export function parseChecksumsManifest(text) {
  if (typeof text !== "string" || text.length === 0) {
    throw new Error("SHA256SUMS manifest is empty");
  }
  if (text.includes("\r")) {
    throw new Error("SHA256SUMS manifest contains CR line endings");
  }
  if (!text.endsWith("\n")) {
    throw new Error("SHA256SUMS manifest must end with a newline");
  }
  /** @type {Map<string, string>} */
  const entries = new Map();
  const lines = text.slice(0, -1).split("\n");
  for (const [index, line] of lines.entries()) {
    const match = /^([a-f0-9]{64}) {2}(\S+)$/.exec(line);
    if (!match) {
      throw new Error(`SHA256SUMS manifest line ${index + 1} is malformed`);
    }
    const [, hex, name] = match;
    if (entries.has(name)) {
      throw new Error(`SHA256SUMS manifest has duplicate entry for ${name}`);
    }
    entries.set(name, `sha256:${hex}`);
  }
  if (entries.size === 0) {
    throw new Error("SHA256SUMS manifest has no entries");
  }
  return entries;
}

/**
 * Verifies that `manifestText` is a canonical, complete `SHA256SUMS.txt`
 * covering every expected installer and both SBOMs for `version` (and never
 * itself or any unknown file), matching every asset's GitHub SHA-256 digest
 * and exact byte-for-byte reproducible formatting.
 *
 * @param {{ version: string, assets: Array<{ name: string, digest: string }>, manifestText: string }} input
 * @returns {Map<string, string>}
 */
export function verifyChecksumsManifest({ version, assets, manifestText }) {
  const names = supplementNames(version);
  const expectedCovered = [...expectedInstallerNames(version), names.spdx, names.cargo].sort(compareCodePoints);
  const byName = new Map(assets.map(asset => [asset.name, asset.digest.toLowerCase()]));
  const parsed = parseChecksumsManifest(manifestText);
  const missing = expectedCovered.filter(name => !parsed.has(name));
  const unexpected = [...parsed.keys()].filter(name => !expectedCovered.includes(name));
  if (missing.length > 0 || unexpected.length > 0) {
    throw new Error(`SHA256SUMS manifest coverage is not exact (missing: ${missing.join(", ") || "none"}; unexpected: ${unexpected.join(", ") || "none"})`);
  }
  /** @type {Array<{ name: string, digest: string }>} */
  const canonicalEntries = [];
  for (const name of expectedCovered) {
    const expectedDigest = byName.get(name);
    if (!expectedDigest) {
      throw new Error(`Draft is missing covered asset ${name} referenced by SHA256SUMS manifest`);
    }
    const listedDigest = parsed.get(name);
    if (listedDigest !== expectedDigest) {
      throw new Error(`SHA256SUMS entry for ${name} disagrees with GitHub digest (${listedDigest} vs ${expectedDigest})`);
    }
    canonicalEntries.push({ name, digest: expectedDigest });
  }
  const canonical = formatChecksumsManifest(canonicalEntries);
  if (manifestText !== canonical) {
    throw new Error("SHA256SUMS manifest is not in canonical sorted format");
  }
  return parsed;
}

/** @param {string} file */
function sha256File(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

/** @param {unknown} value @returns {Record<string, unknown>} */
function record(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Expected a JSON object");
  return Object.fromEntries(Object.entries(value));
}

/**
 * `sboms` carries the two SBOM documents the attest stage uploads, as text.
 * `finalize` downloads the bundled installers into `downloadDir` and opens them
 * for every name in `binaries`; it refuses to run without either. `fetchAsset`
 * and `inspect` are the download and the inspection, injectable so a test needs
 * neither the network nor 7-Zip.
 *
 * @param {{stage: string, repo: string, tag: string, commit: string, releaseId?: string, notes?: string, sboms?: {spdx: string, cargo: string},
 *   downloadDir?: string, binaries?: string[], fetchAsset?: AssetFetcher, inspect?: typeof inspectBundleContents,
 *   now?: () => number, bundleBudgetMs?: number}} options
 * @param {Runner} [run]
 */
export function runReleaseStage(options, run = runCommand) {
  const { stage, repo, tag, commit, releaseId, notes, sboms, downloadDir, binaries, fetchAsset = fetchReleaseAsset, inspect = inspectBundleContents, now = Date.now, bundleBudgetMs = BUNDLE_CHECK_BUDGET_MS } = options;
  if (!["prepare", "check", "attest", "finalize", "ready"].includes(stage)) throw new Error("Expected prepare, check, attest, finalize, or ready");
  if (!/^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9_][A-Za-z0-9_.-]*$/.test(repo)) throw new Error("Invalid repository");
  const parsed = parseTag(tag);
  if (!parsed.ok) throw new Error(parsed.reason);
  const releaseVersion = parsed.version;
  // A beta tag (vX.Y.Z-beta.N) is drafted as a GitHub pre-release from the
  // start, so publishing the draft can never make it the "Latest" release.
  const prerelease = parsed.prerelease;
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error("Expected the full preflight commit SHA");
  if (stage !== "prepare" && stage !== "ready" && !/^[1-9]\d*$/.test(releaseId ?? "")) throw new Error("Expected the prepared release ID");
  if (stage === "finalize" && (!notes || Buffer.byteLength(notes) > 125_000)) throw new Error("Missing or oversized release notes");
  if (stage === "finalize" && (typeof downloadDir !== "string" || downloadDir === "")) throw new Error("finalize needs RELEASE_DOWNLOAD_DIR to open the bundles");
  if (stage === "finalize" && (!Array.isArray(binaries) || binaries.length === 0)) throw new Error("finalize needs the declared [[bin]] names to look for");
  if (stage === "attest") validateSbomDocuments(sboms);
  const base = `repos/${repo}`;
  /** @param {string} endpoint @param {string} [method] @param {Record<string, unknown>} [body] @param {boolean} [allowMissing] */
  function api(endpoint, method = "GET", body, allowMissing = false) {
    const result = run("gh", ["api", "--include", "--method", method, `${base}/${endpoint}`,
      ...(body ? ["--input", "-"] : [])], body ? JSON.stringify(body) : undefined);
    return response(result, method, endpoint, allowMissing);
  }
  /**
   * Uploads one supplement asset through the draft's own `upload_url`, which
   * must name this repository's prepared release on GitHub's upload host.
   * Retries transient 5xx/429/incomplete upload failures up to
   * `MAX_UPLOAD_ATTEMPTS` after wiping any partial (`state: "starter"`) copy of
   * `name` from the draft. Never retries permanent 4xx errors and never touches
   * any asset other than `name`.
   *
   * @param {Record<string, unknown>} release @param {string} name @param {string} content @param {string} contentType
   */
  function upload(release, name, content, contentType) {
    const prefix = `https://uploads.github.com/${base}/releases/${releaseId}/assets`;
    let current = release;
    for (let attempt = 1; attempt <= MAX_UPLOAD_ATTEMPTS; attempt += 1) {
      const template = typeof current.upload_url === "string" ? current.upload_url : "";
      if (template !== `${prefix}{?name,label}`) throw new Error("Draft upload URL is missing or names another release");
      try {
        const result = run("gh", ["api", "--include", "--method", "POST", `${prefix}?name=${encodeURIComponent(name)}`,
          "-H", `Content-Type: ${contentType}`, "--input", "-"], content);
        return record(response(result, "POST", `upload ${name}`, false));
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (attempt >= MAX_UPLOAD_ATTEMPTS || !isTransientUploadError(message)) throw error;
        current = checkDraft(record(api(`releases/${releaseId}`)));
        const rawAssets = Array.isArray(current.assets) ? current.assets : [];
        let deleted = false;
        for (const raw of rawAssets) {
          const metadata = record(raw);
          if (String(metadata.name ?? "") !== name) continue;
          if (typeof metadata.id !== "number" || !Number.isSafeInteger(metadata.id) || metadata.id <= 0) {
            throw new Error("Release asset ID is missing or invalid");
          }
          api(`releases/assets/${metadata.id}`, "DELETE", undefined, true);
          deleted = true;
        }
        if (deleted) current = checkDraft(record(api(`releases/${releaseId}`)));
      }
    }
    throw new Error(`GitHub POST upload ${name}: exceeded retry attempts`);
  }
  /**
   * @param {{status: number | null, stdout: string, failed: boolean}} result
   * @param {string} method @param {string} endpoint @param {boolean} allowMissing
   */
  function response(result, method, endpoint, allowMissing) {
    const statusLine = /^HTTP\/[\d.]+ (\d{3})\b/.exec(result.stdout ?? "");
    if (result.failed || !statusLine) throw new Error(`GitHub ${method} ${endpoint}: incomplete response`);
    const status = Number(statusLine[1]);
    // DELETE /releases/assets/{id} is 204 with an empty body. Parsing that as
    // JSON turns a successful wipe into "unexpected end of JSON input" after
    // the first leftover Windows installer is gone and the rest still 422.
    if (status === 204) {
      if (result.status !== 0) throw new Error(`GitHub ${method} ${endpoint}: HTTP ${status}`);
      return null;
    }
    const match = /^HTTP\/[\d.]+ (\d{3})[^\n]*\r?\n[\s\S]*?\r?\n\r?\n([\s\S]*)$/.exec(result.stdout ?? "");
    if (!match) throw new Error(`GitHub ${method} ${endpoint}: incomplete response`);
    if (status === 404 && allowMissing) return null;
    if (result.status !== 0 || status < 200 || status >= 300) throw new Error(`GitHub ${method} ${endpoint}: HTTP ${status}`);
    return JSON.parse(match[2]);
  }
  function checkCheckout() {
    const local = run("git", ["rev-parse", "HEAD"]);
    if (local.failed || local.status !== 0 || local.stdout.trim() !== commit) throw new Error("Checkout differs from preflight commit");
  }
  function checkTag() {
    checkCheckout();
    const remote = run("git", ["ls-remote", "--exit-code", "origin", `refs/tags/${tag}`, `refs/tags/${tag}^{}`]);
    if (remote.failed || remote.status !== 0) throw new Error("Remote release tag could not be verified");
    const refs = remote.stdout.trim().split(/\r?\n/).map(line => line.split(/\s+/));
    if (refs.length < 1 || refs.length > 2 || refs.some(([sha, ref, extra]) => extra || !/^[a-f0-9]{40}$/.test(sha) || ![`refs/tags/${tag}`, `refs/tags/${tag}^{}`].includes(ref))) throw new Error("Malformed remote tag response");
    const tagged = refs.find(([, ref]) => ref === `refs/tags/${tag}`);
    const peeled = refs.find(([, ref]) => ref === `refs/tags/${tag}^{}`);
    if (!tagged || (peeled ?? tagged)[0] !== commit) throw new Error("Remote release tag moved or names another commit");
    return { object: tagged[0].toLowerCase(), peeled: (peeled ?? tagged)[0].toLowerCase() };
  }
  /**
   * Prepare's 10-minute budget cannot wait out a 30-minute CI matrix. An in-flight
   * run used to look identical to "CI never started", so a tag pushed with main
   * burned preflight and then died here. Name the three states separately.
   * `ready` is the local check that must pass *before* the tag is moved.
   * @param {string} workflow
   */
  function requireSuccessfulPushRun(workflow) {
    const runs = record(api(`actions/workflows/${workflow}/runs?head_sha=${commit}&event=push&per_page=1`));
    const entries = runs.workflow_runs;
    if (!Array.isArray(entries) || entries.length === 0) {
      throw new Error(`${workflow} has no push run for the preflight commit`);
    }
    if (entries.length !== 1) throw new Error(`${workflow} returned an unexpected run page for the preflight commit`);
    const latest = record(entries[0]);
    if (latest.head_sha !== commit || latest.event !== "push") {
      throw new Error(`${workflow} has no push run for the preflight commit`);
    }
    if (latest.status !== "completed") {
      throw new Error(`${workflow} is still ${String(latest.status)} for the preflight commit — wait for it to succeed before tagging`);
    }
    if (latest.conclusion !== "success") {
      throw new Error(`${workflow} latest push run concluded ${String(latest.conclusion)} for the preflight commit`);
    }
    const runId = latest.id;
    if (typeof runId !== "number" || !Number.isSafeInteger(runId) || runId <= 0) {
      throw new Error(`${workflow} push run has no valid numeric ID`);
    }
    const requiredJobs = REQUIRED_WORKFLOW_JOBS[workflow];
    if (!requiredJobs || requiredJobs.length === 0) {
      throw new Error(`No required job contract is declared for ${workflow}`);
    }
    /** @type {Array<{id: unknown, name: unknown, status: unknown, conclusion: unknown, head_sha: unknown}>} */
    const jobs = [];
    /** @type {Set<number>} */
    const seenJobIds = new Set();
    /** @type {number | null} */
    let expectedTotal = null;
    let complete = false;
    for (let page = 1; page <= MAX_API_PAGES; page += 1) {
      const query = page === 1
        ? `actions/runs/${runId}/jobs?filter=latest&per_page=100`
        : `actions/runs/${runId}/jobs?filter=latest&per_page=100&page=${page}`;
      const jobsPage = record(api(query));
      const pageJobs = jobsPage.jobs;
      if (!Array.isArray(pageJobs)) {
        throw new Error(`${workflow} run ${runId} returned a malformed job list`);
      }
      if (jobsPage.total_count !== undefined) {
        if (typeof jobsPage.total_count !== "number" || !Number.isSafeInteger(jobsPage.total_count) || jobsPage.total_count < 0) {
          throw new Error(`${workflow} run ${runId} returned a malformed job total_count`);
        }
        if (expectedTotal !== null && jobsPage.total_count !== expectedTotal) {
          throw new Error(`${workflow} run ${runId} job list changed during pagination (${expectedTotal} -> ${jobsPage.total_count})`);
        }
        expectedTotal = jobsPage.total_count;
      }
      for (const raw of pageJobs) {
        const entry = record(raw);
        if (typeof entry.id === "number" && Number.isSafeInteger(entry.id) && entry.id > 0) {
          if (seenJobIds.has(entry.id)) continue;
          seenJobIds.add(entry.id);
        }
        jobs.push({ id: entry.id, name: entry.name, status: entry.status, conclusion: entry.conclusion, head_sha: entry.head_sha });
      }
      if (pageJobs.length < 100) {
        complete = true;
        break;
      }
      if (expectedTotal !== null && jobs.length >= expectedTotal) {
        complete = true;
        break;
      }
    }
    if (!complete || (expectedTotal !== null && jobs.length !== expectedTotal)) {
      throw new Error(`${workflow} run ${runId} job list was truncated (${jobs.length}/${expectedTotal ?? "unknown"}); cannot prove every required check succeeded`);
    }
    /** @type {Map<string, Array<{status: unknown, conclusion: unknown, head_sha: unknown}>>} */
    const byName = new Map();
    for (const entry of jobs) {
      if (typeof entry.name !== "string" || entry.name === "") continue;
      const list = byName.get(entry.name) ?? [];
      list.push({ status: entry.status, conclusion: entry.conclusion, head_sha: entry.head_sha });
      byName.set(entry.name, list);
    }
    for (const jobName of requiredJobs) {
      const matched = byName.get(jobName);
      if (!matched || matched.length === 0) {
        throw new Error(`${workflow} run ${runId} is missing required job "${jobName}"`);
      }
      for (const entry of matched) {
        if (entry.head_sha !== commit) {
          throw new Error(`${workflow} run ${runId} job "${jobName}" ran against ${String(entry.head_sha)}, not ${commit}`);
        }
        if (entry.status !== "completed" || entry.conclusion !== "success") {
          throw new Error(`${workflow} run ${runId} required job "${jobName}" did not succeed (status=${String(entry.status)}, conclusion=${String(entry.conclusion)})`);
        }
      }
    }
  }
  if (stage === "ready") {
    checkCheckout();
    for (const workflow of ["ci.yml", "coverage.yml"]) requireSuccessfulPushRun(workflow);
    return { release_id: "", commit, tag, stage };
  }
  const tagIdentity = checkTag();
  /** @param {Record<string, unknown> | null} release */
  function assertMutableDraft(release) {
    if (!release || release.draft !== true || release.prerelease !== prerelease || release.immutable === true || release.published_at !== null) throw new Error("Release is not a mutable unpublished draft");
    if (typeof release.id !== "number" || !Number.isSafeInteger(release.id) || release.id <= 0 || (releaseId && String(release.id) !== releaseId)) throw new Error("Draft release ID changed or is invalid");
    return release;
  }
  /** @param {Record<string, unknown>} release */
  function assertDraftTag(release) {
    if (typeof release.tag_name !== "string" || (release.tag_name !== tag && !/^untagged-[0-9a-f]+$/i.test(release.tag_name))) {
      throw new Error(`Draft tag differs from preflight (${String(release.tag_name)})`);
    }
  }
  /** @param {Record<string, unknown> | null} release */
  function checkDraft(release) {
    release = assertMutableDraft(release);
    assertDraftTag(release);
    // GitHub keeps the SHA we POST, then often echoes a branch or tag name once
    // the existing git tag is associated. v0.0.9's finalize died on that rewrite
    // after every installer had uploaded. The remote tag peel is the pin;
    // a 40-character SHA here must still be this commit or the annotated tag
    // object, and a ref name is not a second pin.
    //
    // After those uploads GitHub can also detach the git tag from the draft and
    // rewrite tag_name to untagged-<hex>. That rewrite aborted finalize once
    // every installer was already on the draft. The peel is the pin; finalize
    // writes the intended tag name back with the notes.
    if (typeof release.target_commitish !== "string" || !release.target_commitish) throw new Error("Draft commitish is missing");
    if (/^[a-f0-9]{7,40}$/i.test(release.target_commitish)) {
      const sha = release.target_commitish.toLowerCase();
      const pins = [commit, tagIdentity.object, tagIdentity.peeled];
      if (!pins.some(pin => pin === sha || pin.startsWith(sha))) {
        throw new Error(`Draft commit SHA differs from preflight (${release.target_commitish})`);
      }
    }
    return release;
  }
  /** @param {Record<string, unknown>} release */
  function assetSnapshot(release) {
    const inventory = inspectReleaseAssets({ tag, json: release });
    if (!inventory.ok) throw new Error(`Incomplete release assets: ${inventory.violations.join("; ")}`);
    return JSON.stringify(assetDigests(release));
  }
  /**
   * Every asset's id, name and GitHub-computed SHA-256, sorted by code point.
   * @param {Record<string, unknown>} release
   */
  function assetDigests(release) {
    if (!Array.isArray(release.assets)) throw new Error("Missing asset metadata");
    if (release.assets.length >= 100) throw new Error("Release asset list reached 100 entries and may be truncated");
    const assets = release.assets.map(asset => {
      const metadata = record(asset);
      if (typeof metadata.id !== "number" || !Number.isSafeInteger(metadata.id) || metadata.id <= 0 ||
          typeof metadata.digest !== "string" || !/^sha256:[a-f0-9]{64}$/i.test(metadata.digest)) throw new Error("Release asset ID or SHA-256 metadata is missing or invalid");
      return {id: metadata.id, name: String(metadata.name), size: metadata.size, state: metadata.state, digest: metadata.digest.toLowerCase()};
    }).sort((a, b) => compareCodePoints(a.name, b.name));
    if (new Set(assets.map(asset => asset.id)).size !== assets.length) throw new Error("Duplicate release asset IDs");
    return assets;
  }
  /**
   * Proves the draft's `SHA256SUMS.txt` asset metadata (byte size and GitHub
   * SHA-256 digest) equals the exact canonical manifest derived from all other
   * assets on the draft before spending bandwidth downloading installers.
   *
   * @param {Record<string, unknown>} release
   */
  function verifyChecksumsMetadata(release) {
    const names = supplementNames(releaseVersion);
    const allAssets = assetDigests(release);
    const sumsAsset = allAssets.find(asset => asset.name === names.checksums);
    if (!sumsAsset) throw new Error(`Checksums manifest ${names.checksums} is not on the draft`);
    const covered = allAssets.filter(asset => asset.name !== names.checksums);
    const expectedSums = formatChecksumsManifest(covered.map(asset => ({ name: asset.name, digest: asset.digest })));
    const expectedBytes = Buffer.byteLength(expectedSums, "utf8");
    const expectedDigest = `sha256:${createHash("sha256").update(expectedSums, "utf8").digest("hex")}`;
    if (sumsAsset.size !== expectedBytes || sumsAsset.digest !== expectedDigest) {
      throw new Error(`SHA256SUMS manifest ${names.checksums} does not match the current draft assets (expected ${expectedDigest}/${expectedBytes}B, got ${sumsAsset.digest}/${String(sumsAsset.size)}B)`);
    }
    return { sumsAsset, allAssets };
  }
  /**
   * Downloads each bundled installer and `SHA256SUMS.txt` by asset ID, proves
   * the bytes are the ones GitHub holds (size and SHA-256 from the draft's
   * metadata) before opening them, verifies `SHA256SUMS.txt` parses cleanly and
   * agrees with every draft asset and downloaded installer, requires every
   * declared binary inside each bundle, and proves the files did not change
   * while being read. Returns the coverage statement.
   * @param {Record<string, unknown>} release
   */
  function checkBundleContents(release) {
    const { sumsAsset, allAssets } = verifyChecksumsMetadata(release);
    const byName = new Map(allAssets.map(asset => [asset.name, asset]));
    mkdirSync(String(downloadDir), {recursive: true});
    const work = mkdtempSync(path.join(String(downloadDir), "bundles-"));
    try {
      /** @type {Record<string, string>} */
      const files = {};
      /** @type {Record<string, string>} */
      const digests = {};
      // Every size is judged before the first byte is fetched.
      const wanted = bundleSpecs(releaseVersion).map(spec => {
        const asset = byName.get(spec.asset);
        if (!asset) throw new Error(`Bundle ${spec.asset} is not on the draft`);
        const {id, size, digest} = asset;
        if (typeof size !== "number" || !Number.isSafeInteger(size) || size <= 0 || size > MAX_ASSET_BYTES) {
          throw new Error(`Bundle ${spec.asset} has a size outside 1..${MAX_ASSET_BYTES}`);
        }
        return {spec, id, size, digest};
      });
      const sumsSize = sumsAsset.size;
      if (typeof sumsSize !== "number" || !Number.isSafeInteger(sumsSize) || sumsSize <= 0 || sumsSize > MAX_ASSET_BYTES) {
        throw new Error(`SHA256SUMS manifest ${sumsAsset.name} has a size outside 1..${MAX_ASSET_BYTES}`);
      }
      const budget = createBudget({totalMs: bundleBudgetMs, now});
      for (const [done, {spec, id, size, digest}] of wanted.entries()) {
        const dest = path.join(work, spec.asset);
        /** @type {number} */
        let timeoutMs;
        try {
          timeoutMs = budget.callTimeout(DOWNLOAD_TIMEOUT_MS);
        } catch (error) {
          if (error instanceof BudgetExhausted) throw new Error(`bundle check budget exhausted after ${done} of ${wanted.length} bundles`);
          throw error;
        }
        try {
          fetchAsset({repo, asset: {id, name: spec.asset, size, digest}, dest, timeoutMs});
        } catch (error) {
          // A download killed by the clamped limit is the budget running out.
          try { budget.callTimeout(1); } catch (exhausted) {
            if (exhausted instanceof BudgetExhausted) throw new Error(`bundle check budget exhausted after ${done} of ${wanted.length} bundles`);
            throw exhausted;
          }
          throw error;
        }
        const got = statSync(dest).size;
        if (got !== size) throw new Error(`Bundle ${spec.asset} downloaded ${got} bytes, expected ${size}`);
        if (`sha256:${sha256File(dest)}` !== digest) throw new Error(`Bundle ${spec.asset} does not match GitHub's SHA-256`);
        files[spec.asset] = dest;
        digests[spec.asset] = digest;
      }
      // Download and parse SHA256SUMS.txt directly so verify independently proves
      // the stored manifest bytes match every asset and every locally hashed bundle.
      const sumsDest = path.join(work, sumsAsset.name);
      /** @type {number} */
      let sumsTimeoutMs;
      try {
        sumsTimeoutMs = budget.callTimeout(DOWNLOAD_TIMEOUT_MS);
      } catch (error) {
        if (error instanceof BudgetExhausted) throw new Error("bundle check budget exhausted before SHA256SUMS verification");
        throw error;
      }
      try {
        fetchAsset({repo, asset: {id: sumsAsset.id, name: sumsAsset.name, size: sumsSize, digest: sumsAsset.digest}, dest: sumsDest, timeoutMs: sumsTimeoutMs});
      } catch (error) {
        try { budget.callTimeout(1); } catch (exhausted) {
          if (exhausted instanceof BudgetExhausted) throw new Error("bundle check budget exhausted before SHA256SUMS verification");
          throw exhausted;
        }
        throw error;
      }
      const sumsGot = statSync(sumsDest).size;
      if (sumsGot !== sumsSize) throw new Error(`SHA256SUMS manifest ${sumsAsset.name} downloaded ${sumsGot} bytes, expected ${sumsSize}`);
      if (`sha256:${sha256File(sumsDest)}` !== sumsAsset.digest) throw new Error(`SHA256SUMS manifest ${sumsAsset.name} does not match GitHub's SHA-256`);
      const parsedSums = verifyChecksumsManifest({ version: releaseVersion, assets: allAssets, manifestText: readFileSync(sumsDest, "utf8") });
      for (const [name, localDigest] of Object.entries(digests)) {
        if (parsedSums.get(name) !== localDigest) {
          throw new Error(`Downloaded bundle ${name} does not match ${sumsAsset.name}`);
        }
      }
      const result = inspect({version: releaseVersion, binaries: binaries ?? [], files, workDir: work, budget});
      if (!result.ok) throw new Error(`Bundle contents are wrong: ${result.violations.join("; ")} (${result.coverage})`);
      for (const [name, file] of Object.entries(files)) {
        if (`sha256:${sha256File(file)}` !== digests[name]) throw new Error(`Bundle ${name} changed while it was being inspected`);
      }
      if (`sha256:${sha256File(sumsDest)}` !== sumsAsset.digest) throw new Error(`SHA256SUMS manifest ${sumsAsset.name} changed while bundles were being inspected`);
      return result.coverage;
    } finally {
      rmSync(work, {recursive: true, force: true});
    }
  }
  /**
   * `/releases/tags/{tag}` only returns published releases. A draft created for
   * this tag is invisible there, and after installer uploads GitHub can also
   * rewrite `tag_name` to `untagged-<hex>`. Looking up only by tag then POSTs
   * a second draft while the complete asset set sits on the first. Paginate
   * releases (up to `MAX_API_PAGES` pages of 100) and refuse if still truncated
   * rather than treating a truncated view as "no matching draft".
   */
  function findExistingDraft() {
    const byTag = api(`releases/tags/${tag}`, "GET", undefined, true);
    if (byTag) return record(byTag);
    /** @type {Record<string, unknown>[]} */
    const matches = [];
    /** @type {Set<number>} */
    const seenReleaseIds = new Set();
    let complete = false;
    for (let page = 1; page <= MAX_API_PAGES; page += 1) {
      const query = page === 1 ? "releases?per_page=100" : `releases?per_page=100&page=${page}`;
      const listed = api(query);
      if (!Array.isArray(listed)) throw new Error("Release list is not an array");
      for (const entry of listed) {
        const candidate = record(entry);
        if (typeof candidate.id === "number" && Number.isSafeInteger(candidate.id) && candidate.id > 0) {
          if (seenReleaseIds.has(candidate.id)) continue;
          seenReleaseIds.add(candidate.id);
        }
        if (candidate.draft !== true) continue;
        // `/releases/tags/{tag}` never returns drafts, even while tag_name is
        // still this tag. Resume those by tag_name. After uploads GitHub may
        // rewrite tag_name to untagged-<hex>; those are the ones we POST as
        // `GitPulse ${tag}` and can only match by that name.
        if (candidate.name !== `GitPulse ${tag}` && candidate.tag_name !== tag) continue;
        matches.push(candidate);
      }
      if (listed.length < 100) {
        complete = true;
        break;
      }
    }
    if (!complete) {
      throw new Error(`Release list was capped at ${MAX_API_PAGES * 100} across ${MAX_API_PAGES} pages; cannot prove a matching draft is absent or unique`);
    }
    if (matches.length > 1) throw new Error(`Multiple matching drafts for ${tag}`);
    return matches[0] ?? null;
  }
  let release;
  if (stage === "prepare") {
    // The newest run must have completed successfully, including every matrix
    // leg. An older successful attempt cannot hide a newer failure/cancellation.
    for (const workflow of ["ci.yml", "coverage.yml"]) requireSuccessfulPushRun(workflow);
    release = findExistingDraft();
    if (!release) {
      // Do not automatically retry a POST whose outcome is unknown. A rerun
      // reads the existing draft before deciding whether creation is necessary.
      release = record(api("releases", "POST", { tag_name: tag, target_commitish: commit, name: `GitPulse ${tag}`, draft: true, prerelease, body: "Draft — platform builds and verification are pending." }));
    } else {
      // Identity first. `/releases/tags/{tag}` can still return a published
      // release; wiping its installers would delete the last successful set.
      release = assertMutableDraft(release);
      assertDraftTag(release);
      // The matrix always rebuilds. Leftover names — especially the Windows
      // NSIS/MSI pair from a cancelled attempt — make GitHub 422
      // already_exists. tauri-action retries that three times and then fails
      // the Windows leg with every other platform already uploaded. Wipe the
      // previous attempt's installers across bounded passes before the matrix
      // runs (handles inline asset lists >30/100 safely).
      for (let pass = 0; pass < MAX_WIPE_PASSES; pass += 1) {
        const assets = Array.isArray(release.assets) ? release.assets : [];
        if (assets.length === 0) break;
        for (const raw of assets) {
          const metadata = record(raw);
          if (typeof metadata.id !== "number" || !Number.isSafeInteger(metadata.id) || metadata.id <= 0) {
            throw new Error("Release asset ID is missing or invalid");
          }
          api(`releases/assets/${metadata.id}`, "DELETE", undefined, true);
        }
        release = assertMutableDraft(record(api(`releases/${release.id}`)));
        assertDraftTag(release);
      }
      if (Array.isArray(release.assets) && release.assets.length > 0) {
        throw new Error("Draft still has installer assets after wipe");
      }
      // A draft still carrying the previous tagged SHA is that earlier tree.
      // checkDraft would refuse it after a tag move, which is the only draft
      // we can resume. Retarget onto this commit; a ref name is left alone.
      const commitish = typeof release.target_commitish === "string" ? release.target_commitish : "";
      const shaMismatch = /^[a-f0-9]{7,40}$/i.test(commitish) &&
        ![commit, tagIdentity.object, tagIdentity.peeled].some(pin => pin === commitish.toLowerCase() || pin.startsWith(commitish.toLowerCase()));
      const untagged = typeof release.tag_name === "string" && /^untagged-[0-9a-f]+$/i.test(release.tag_name);
      if (untagged || shaMismatch) {
        release = record(api(`releases/${release.id}`, "PATCH", { tag_name: tag, target_commitish: commit, name: `GitPulse ${tag}` }));
      }
    }
  } else {
    release = record(api(`releases/${releaseId}`));
  }
  release = checkDraft(release);
  if (stage === "attest") {
    const version = parsed.version;
    const names = supplementNames(version);
    const supplements = new Set(Object.values(names));
    // A rerun of this stage finds its own earlier uploads — including partial
    // `state: "starter"` uploads with null digests left by an interrupted run.
    // Inspect raw assets by name/id before demanding installer digests so a
    // rerun always recovers cleanly without ever deleting an installer.
    if (!Array.isArray(release.assets)) throw new Error("Missing asset metadata");
    /** @type {number[]} */
    const leftoverIds = [];
    for (const raw of release.assets) {
      const metadata = record(raw);
      if (!supplements.has(String(metadata.name ?? ""))) continue;
      if (typeof metadata.id !== "number" || !Number.isSafeInteger(metadata.id) || metadata.id <= 0) {
        throw new Error("Release asset ID is missing or invalid");
      }
      leftoverIds.push(metadata.id);
    }
    for (const id of leftoverIds) api(`releases/assets/${id}`, "DELETE", undefined, true);
    if (leftoverIds.length > 0) release = checkDraft(record(api(`releases/${releaseId}`)));
    const installers = assetDigests(release);
    for (const asset of installers) {
      if (asset.state !== "uploaded") {
        throw new Error(`Installer ${asset.name} is not uploaded (state=${String(asset.state)})`);
      }
      if (typeof asset.size !== "number" || !Number.isSafeInteger(asset.size) || asset.size <= 0) {
        throw new Error(`Installer ${asset.name} has invalid size (${String(asset.size)})`);
      }
    }
    const wanted = expectedInstallerNames(version);
    const present = installers.map(asset => asset.name);
    const seenInstallers = new Set();
    /** @type {string[]} */
    const duplicates = [];
    for (const name of present) {
      if (seenInstallers.has(name)) duplicates.push(name);
      seenInstallers.add(name);
    }
    const missing = wanted.filter(name => !seenInstallers.has(name));
    const unexpected = present.filter(name => !wanted.includes(name));
    if (duplicates.length > 0 || missing.length > 0 || unexpected.length > 0) {
      throw new Error(`Installer set is not exact (missing: ${missing.join(", ") || "none"}; unexpected: ${unexpected.join(", ") || "none"}${duplicates.length > 0 ? `; duplicate: ${duplicates.join(", ")}` : ""})`);
    }
    /** @param {string} text */
    const sha256 = text => createHash("sha256").update(text, "utf8").digest("hex");
    const local = new Map([[names.spdx, sha256(sboms?.spdx ?? "")], [names.cargo, sha256(sboms?.cargo ?? "")]]);
    // GitHub's digest is computed over the stored bytes, which are exactly
    // what a user downloads; the SBOM digests are checked against it below.
    const sums = formatChecksumsManifest([
      ...installers.map(asset => ({ name: asset.name, digest: asset.digest })),
      ...[...local].map(([name, hash]) => ({ name, digest: `sha256:${hash}` })),
    ]);
    local.set(names.checksums, sha256(sums));
    upload(release, names.spdx, sboms?.spdx ?? "", "application/spdx+json");
    upload(release, names.cargo, sboms?.cargo ?? "", "application/vnd.cyclonedx+json");
    upload(release, names.checksums, sums, "text/plain");
    const confirmed = checkDraft(record(api(`releases/${releaseId}`)));
    // The full set, installers and supplements, must now be exact.
    assetSnapshot(confirmed);
    const uploaded = new Map(assetDigests(confirmed).map(asset => [asset.name, asset.digest]));
    for (const asset of installers) {
      if (uploaded.get(asset.name) !== asset.digest) throw new Error(`Installer ${asset.name} changed during attestation`);
    }
    for (const [name, hash] of local) {
      if (uploaded.get(name) !== `sha256:${hash}`) throw new Error(`Uploaded ${name} does not match its local SHA-256`);
    }
    checkTag();
  }
  /** @type {string | undefined} */
  let coverage;
  if (stage === "finalize") {
    const assets = assetSnapshot(release);
    verifyChecksumsMetadata(release);
    checkTag();
    coverage = checkBundleContents(release);
    checkDraft(record(api(`releases/${releaseId}`)));
    checkDraft(record(api(`releases/${releaseId}`, "PATCH", { tag_name: tag, body: notes })));
    const confirmed = checkDraft(record(api(`releases/${releaseId}`)));
    /** @param {unknown} [text] */
    const normalizeNotes = (text) => (typeof text === "string" ? text : "").replace(/\r\n/g, "\n").trim();
    if (confirmed.body !== notes && normalizeNotes(confirmed.body) !== normalizeNotes(notes)) {
      throw new Error("Release notes round trip differs from changelog");
    }
    if (assetSnapshot(confirmed) !== assets) throw new Error("Release assets changed during finalization");
    checkTag();
  }
  return { release_id: String(release.id), commit, tag, stage, ...(coverage ? { bundles: coverage } : {}) };
}

/**
 * The three identifiers a stage needs. CI supplies all of them; a local
 * `ready` check supplies none, so each falls back to what this checkout
 * already knows.
 *
 * Both the environment and the command runner are parameters so this can be
 * exercised without spawning anything. A test that shelled out here measured
 * the host rather than the resolution: three `git` spawns plus a `gh` call
 * reaching the network sat under Vitest's 5s limit on Linux and macOS and
 * exceeded it on a Windows runner, where process creation is dearer — a
 * failure that said nothing about the code under test.
 *
 * @param {Record<string, string | undefined>} [env]
 * @param {Runner} [run]
 * @returns {{repo: string, commit: string, tag: string}}
 */
export function resolveReleaseContext(env = process.env, run = runCommand) {
  let repo = env.GH_REPO ?? "";
  if (!repo) {
    const origin = run("git", ["config", "--get", "remote.origin.url"]).stdout.trim();
    const match = /(?:github\.com[:/])([^/]+)\/([^/.]+?)(?:\.git)?$/.exec(origin);
    if (match) repo = `${match[1]}/${match[2]}`;
  }
  let commit = env.RELEASE_COMMIT ?? "";
  if (!commit) {
    commit = run("git", ["rev-parse", "HEAD"]).stdout.trim();
  }
  let tag = env.RELEASE_TAG ?? "";
  if (!tag) {
    const tagsAtHead = run("git", ["tag", "--points-at", "HEAD"]).stdout.trim().split(/\s+/).filter(Boolean);
    const validTags = tagsAtHead.flatMap((t) => {
      const parsed = parseTag(t);
      return parsed.ok ? [{ tag: t, prerelease: parsed.prerelease }] : [];
    });
    const preferred = validTags.find((entry) => !entry.prerelease) ?? validTags[0];
    if (preferred) {
      tag = preferred.tag;
    } else {
      try {
        const pkg = JSON.parse(readFileSync("package.json", "utf8"));
        if (pkg.version) tag = `v${pkg.version}`;
      } catch {
        // An unreadable package.json leaves the tag empty rather than guessed;
        // runReleaseStage then refuses it by name.
      }
    }
  }
  return {repo, commit, tag};
}

/** @param {string[]} [argv] */
export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(formatUsage({name: "release-state", summary: "Verify remote release provenance and manage its draft lifecycle.",
      flags: [{flag: "prepare|check|attest|finalize|ready", description: "stage to run; uses GH_REPO, RELEASE_TAG, RELEASE_COMMIT and RELEASE_ID. attest also reads the SBOM files named by RELEASE_SBOM_SPDX and RELEASE_SBOM_CARGO. finalize also downloads the bundled installers into RELEASE_DOWNLOAD_DIR and opens them (needs gh and 7z). ready checks CI without mutating the draft or requiring the tag to already point here"}],
      exits: "0 stage verified; 1 release refused or verification unavailable"}));
    return 0;
  }
  try {
    if (argv.length !== 1) throw new Error("Usage: release-state.mjs prepare|check|attest|finalize|ready (RELEASE_TAG, RELEASE_COMMIT, GH_REPO, RELEASE_ID)");
    const {repo, commit, tag} = resolveReleaseContext();
    let notes;
    if (argv[0] === "finalize") {
      const section = extractNotes(readFileSync("CHANGELOG.md", "utf8"), tag);
      if (!section.found) throw new Error("Changelog has no notes for release tag");
      notes = section.body;
    }
    let binaries;
    if (argv[0] === "finalize") {
      binaries = declaredBinaryNames(readFileSync(path.join("src-tauri", "Cargo.toml"), "utf8"));
    }
    let sboms;
    if (argv[0] === "attest") {
      const spdx = process.env.RELEASE_SBOM_SPDX;
      const cargo = process.env.RELEASE_SBOM_CARGO;
      if (!spdx || !cargo) throw new Error("attest needs RELEASE_SBOM_SPDX and RELEASE_SBOM_CARGO");
      sboms = {spdx: readFileSync(spdx, "utf8"), cargo: readFileSync(cargo, "utf8")};
    }
    const result = runReleaseStage({stage: argv[0], repo, tag,
      commit, releaseId: process.env.RELEASE_ID, notes, sboms, binaries, downloadDir: process.env.RELEASE_DOWNLOAD_DIR});
    if (process.env.GITHUB_OUTPUT && result.release_id) appendFileSync(process.env.GITHUB_OUTPUT, `release_id=${result.release_id}\n`);
    console.log(JSON.stringify(result));
    return 0;
  } catch (error) {
    console.error(`Release refused: ${error instanceof Error ? error.message : "unknown failure"}`);
    return 1;
  }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main();
