#!/usr/bin/env node
/** One owner for remote release identity, CI provenance, and draft finalization. */
import { spawnSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseTag } from "./check-release-version.mjs";
import { inspectReleaseAssets } from "./check-release-assets.mjs";
import { extractNotes } from "./release-notes.mjs";
import { formatUsage, wantsHelp } from "./usage.mjs";

/** @typedef {(program: string, args: string[], input?: string) => {status: number | null, stdout: string, failed: boolean}} Runner */
/** @type {Runner} */
export function runCommand(program, args, input) {
  const result = spawnSync(program, args, {
    input, encoding: "utf8", timeout: 60_000, maxBuffer: 4 * 1024 * 1024,
    env: { ...process.env, GH_PROMPT_DISABLED: "1", GIT_TERMINAL_PROMPT: "0" },
  });
  return { status: result.status, stdout: result.stdout ?? "", failed: !!result.error || !!result.signal };
}

/** @param {unknown} value @returns {Record<string, unknown>} */
function record(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Expected a JSON object");
  return Object.fromEntries(Object.entries(value));
}

/**
 * @param {{stage: string, repo: string, tag: string, commit: string, releaseId?: string, notes?: string}} options
 * @param {Runner} [run]
 */
export function runReleaseStage(options, run = runCommand) {
  const { stage, repo, tag, commit, releaseId, notes } = options;
  if (!["prepare", "check", "finalize"].includes(stage)) throw new Error("Expected prepare, check, or finalize");
  if (!/^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9_][A-Za-z0-9_.-]*$/.test(repo)) throw new Error("Invalid repository");
  const parsed = parseTag(tag);
  if (!parsed.ok) throw new Error(parsed.reason);
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error("Expected the full preflight commit SHA");
  if (stage !== "prepare" && !/^[1-9]\d*$/.test(releaseId ?? "")) throw new Error("Expected the prepared release ID");
  if (stage === "finalize" && (!notes || Buffer.byteLength(notes) > 125_000)) throw new Error("Missing or oversized release notes");
  const base = `repos/${repo}`;
  /** @param {string} endpoint @param {string} [method] @param {Record<string, unknown>} [body] @param {boolean} [allowMissing] */
  function api(endpoint, method = "GET", body, allowMissing = false) {
    const result = run("gh", ["api", "--include", "--method", method, `${base}/${endpoint}`,
      ...(body ? ["--input", "-"] : [])], body ? JSON.stringify(body) : undefined);
    const match = /^HTTP\/[\d.]+ (\d{3})[^\n]*\r?\n[\s\S]*?\r?\n\r?\n([\s\S]*)$/.exec(result.stdout);
    if (result.failed || !match) throw new Error(`GitHub ${method} ${endpoint}: incomplete response`);
    const status = Number(match[1]);
    if (status === 404 && allowMissing && result.status === 1) return null;
    if (result.status !== 0 || status < 200 || status >= 300) throw new Error(`GitHub ${method} ${endpoint}: HTTP ${status}`);
    return JSON.parse(match[2]);
  }
  function checkTag() {
    const local = run("git", ["rev-parse", "HEAD"]);
    if (local.failed || local.status !== 0 || local.stdout.trim() !== commit) throw new Error("Checkout differs from preflight commit");
    const remote = run("git", ["ls-remote", "--exit-code", "origin", `refs/tags/${tag}`, `refs/tags/${tag}^{}`]);
    if (remote.failed || remote.status !== 0) throw new Error("Remote release tag could not be verified");
    const refs = remote.stdout.trim().split(/\r?\n/).map(line => line.split(/\s+/));
    if (refs.length < 1 || refs.length > 2 || refs.some(([sha, ref, extra]) => extra || !/^[a-f0-9]{40}$/.test(sha) || ![`refs/tags/${tag}`, `refs/tags/${tag}^{}`].includes(ref))) throw new Error("Malformed remote tag response");
    const tagged = refs.find(([, ref]) => ref === `refs/tags/${tag}`);
    const peeled = refs.find(([, ref]) => ref === `refs/tags/${tag}^{}`);
    if (!tagged || (peeled ?? tagged)[0] !== commit) throw new Error("Remote release tag moved or names another commit");
    return { object: tagged[0].toLowerCase(), peeled: (peeled ?? tagged)[0].toLowerCase() };
  }
  const tagIdentity = checkTag();
  /** @param {Record<string, unknown> | null} release */
  function checkDraft(release) {
    if (!release || release.draft !== true || release.prerelease !== false || release.immutable === true || release.published_at !== null) throw new Error("Release is not a mutable unpublished draft");
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
    if (typeof release.tag_name !== "string" || (release.tag_name !== tag && !/^untagged-[0-9a-f]+$/i.test(release.tag_name))) {
      throw new Error(`Draft tag differs from preflight (${String(release.tag_name)})`);
    }
    if (typeof release.target_commitish !== "string" || !release.target_commitish) throw new Error("Draft commitish is missing");
    if (/^[a-f0-9]{7,40}$/i.test(release.target_commitish)) {
      const sha = release.target_commitish.toLowerCase();
      const pins = [commit, tagIdentity.object, tagIdentity.peeled];
      if (!pins.some(pin => pin === sha || pin.startsWith(sha))) {
        throw new Error(`Draft commit SHA differs from preflight (${release.target_commitish})`);
      }
    }
    if (typeof release.id !== "number" || !Number.isSafeInteger(release.id) || release.id <= 0 || (releaseId && String(release.id) !== releaseId)) throw new Error("Draft release ID changed or is invalid");
    return release;
  }
  /** @param {Record<string, unknown>} release */
  function assetSnapshot(release) {
    const inventory = inspectReleaseAssets({ tag, json: release });
    if (!inventory.ok) throw new Error(`Incomplete release assets: ${inventory.violations.join("; ")}`);
    if (!Array.isArray(release.assets)) throw new Error("Missing asset metadata");
    const assets = release.assets.map(asset => {
      const metadata = record(asset);
      if (typeof metadata.id !== "number" || !Number.isSafeInteger(metadata.id) || metadata.id <= 0 ||
          typeof metadata.digest !== "string" || !/^sha256:[a-f0-9]{64}$/.test(metadata.digest)) throw new Error("Release asset ID or SHA-256 metadata is missing or invalid");
      return {id: metadata.id, name: String(metadata.name), size: metadata.size, state: metadata.state, digest: metadata.digest};
    }).sort((a, b) => a.name.localeCompare(b.name));
    if (new Set(assets.map(asset => asset.id)).size !== assets.length) throw new Error("Duplicate release asset IDs");
    return JSON.stringify(assets);
  }
  /**
   * `/releases/tags/{tag}` only returns published releases. A draft created for
   * this tag is invisible there, and after installer uploads GitHub can also
   * rewrite `tag_name` to `untagged-<hex>`. Looking up only by tag then POSTs
   * a second draft while the complete asset set sits on the first. List
   * drafts by the name we POST; refuse when the page is full rather than
   * treating a truncated view as "no matching draft".
   */
  function findExistingDraft() {
    const byTag = api(`releases/tags/${tag}`, "GET", undefined, true);
    if (byTag) return record(byTag);
    const listed = api("releases?per_page=100");
    if (!Array.isArray(listed)) throw new Error("Release list is not an array");
    if (listed.length >= 100) throw new Error("Release list was capped at 100; cannot prove a matching draft is absent or unique");
    const matches = [];
    for (const entry of listed) {
      const candidate = record(entry);
      if (candidate.draft !== true) continue;
      // `/releases/tags/{tag}` never returns drafts, even while tag_name is
      // still this tag. Resume those by tag_name. After uploads GitHub may
      // rewrite tag_name to untagged-<hex>; those are the ones we POST as
      // `GitPulse ${tag}` and can only match by that name.
      if (candidate.name !== `GitPulse ${tag}` && candidate.tag_name !== tag) continue;
      matches.push(candidate);
    }
    if (matches.length > 1) throw new Error(`Multiple matching drafts for ${tag}`);
    return matches[0] ?? null;
  }
  let release;
  if (stage === "prepare") {
    // The newest run must have completed successfully, including every matrix
    // leg. An older successful attempt cannot hide a newer failure/cancellation.
    for (const workflow of ["ci.yml", "coverage.yml"]) {
      const runs = record(api(`actions/workflows/${workflow}/runs?head_sha=${commit}&event=push&per_page=1`));
      const entries = runs.workflow_runs;
      const latest = Array.isArray(entries) && entries.length === 1 ? record(entries[0]) : null;
      if (!latest || latest.head_sha !== commit || latest.event !== "push" || latest.status !== "completed" || latest.conclusion !== "success") throw new Error(`${workflow} has no successful latest push run for the preflight commit`);
    }
    release = findExistingDraft();
    if (!release) {
      // Do not automatically retry a POST whose outcome is unknown. A rerun
      // reads the existing draft before deciding whether creation is necessary.
      release = record(api("releases", "POST", { tag_name: tag, target_commitish: commit, name: `GitPulse ${tag}`, draft: true, prerelease: false, body: "Draft — platform builds and verification are pending." }));
    }
  } else {
    release = record(api(`releases/${releaseId}`));
  }
  release = checkDraft(release);
  if (stage === "finalize") {
    const assets = assetSnapshot(release);
    checkTag();
    checkDraft(record(api(`releases/${releaseId}`)));
    checkDraft(record(api(`releases/${releaseId}`, "PATCH", { tag_name: tag, body: notes })));
    const confirmed = checkDraft(record(api(`releases/${releaseId}`)));
    if (confirmed.body !== notes) throw new Error("Release notes round trip differs from changelog");
    if (assetSnapshot(confirmed) !== assets) throw new Error("Release assets changed during finalization");
    checkTag();
  }
  return { release_id: String(release.id), commit, tag, stage };
}

/** @param {string[]} [argv] */
export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(formatUsage({name: "release-state", summary: "Verify remote release provenance and manage its draft lifecycle.",
      flags: [{flag: "prepare|check|finalize", description: "stage to run; uses GH_REPO, RELEASE_TAG, RELEASE_COMMIT and RELEASE_ID"}],
      exits: "0 stage verified; 1 release refused or verification unavailable"}));
    return 0;
  }
  try {
    if (argv.length !== 1) throw new Error("Usage: release-state.mjs prepare|check|finalize (RELEASE_TAG, RELEASE_COMMIT, GH_REPO, RELEASE_ID)");
    const tag = process.env.RELEASE_TAG ?? "";
    let notes;
    if (argv[0] === "finalize") {
      const section = extractNotes(readFileSync("CHANGELOG.md", "utf8"), tag);
      if (!section.found) throw new Error("Changelog has no notes for release tag");
      notes = section.body;
    }
    const result = runReleaseStage({stage: argv[0], repo: process.env.GH_REPO ?? "", tag,
      commit: process.env.RELEASE_COMMIT ?? "", releaseId: process.env.RELEASE_ID, notes});
    if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `release_id=${result.release_id}\n`);
    console.log(JSON.stringify(result));
    return 0;
  } catch (error) {
    console.error(`Release refused: ${error instanceof Error ? error.message : "unknown failure"}`);
    return 1;
  }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = main();
