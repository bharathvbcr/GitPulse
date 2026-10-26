#!/usr/bin/env node
/** One owner for remote release identity, CI provenance, and draft finalization. */
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, closeSync, mkdirSync, mkdtempSync, openSync, readFileSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseTag } from "./check-release-version.mjs";
import { bundleSpecs, declaredBinaryNames, expectedInstallerNames, inspectBundleContents, inspectReleaseAssets, supplementNames } from "./check-release-assets.mjs";
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

/** One installer download may take this long and be this large; both bound the verify job. */
export const DOWNLOAD_TIMEOUT_MS = 120_000;
export const MAX_ASSET_BYTES = 512 * 1024 * 1024;

/**
 * @typedef {{id: number, name: string, size: number, digest: string}} DraftAsset
 * @typedef {(request: {repo: string, asset: DraftAsset, dest: string}) => void} AssetFetcher
 */

/**
 * Streams one draft asset's bytes to `dest` through `gh api`, by asset ID:
 * `/releases/tags/{tag}` cannot see a draft, and `--include` would put the
 * response headers in front of the binary. The file descriptor is the child's
 * stdout, so the bytes never pass through a buffer here. Size and digest are
 * checked by the caller, which owns the bound on what it will open.
 *
 * @type {AssetFetcher}
 */
export function fetchReleaseAsset({repo, asset, dest}) {
  const fd = openSync(dest, "wx", 0o600);
  try {
    const result = spawnSync("gh", ["api", "-H", "Accept: application/octet-stream", `repos/${repo}/releases/assets/${asset.id}`], {
      stdio: ["ignore", fd, "pipe"], encoding: "utf8", timeout: DOWNLOAD_TIMEOUT_MS, maxBuffer: 1024 * 1024,
      env: { ...process.env, GH_PROMPT_DISABLED: "1" },
    });
    if (result.error || result.signal || result.status !== 0) {
      throw new Error(`Download of ${asset.name} failed: ${result.error?.message ?? result.signal ?? (result.stderr ?? "").trim().slice(0, 200)}`);
    }
  } finally {
    closeSync(fd);
  }
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
 *   downloadDir?: string, binaries?: string[], fetchAsset?: AssetFetcher, inspect?: typeof inspectBundleContents}} options
 * @param {Runner} [run]
 */
export function runReleaseStage(options, run = runCommand) {
  const { stage, repo, tag, commit, releaseId, notes, sboms, downloadDir, binaries, fetchAsset = fetchReleaseAsset, inspect = inspectBundleContents } = options;
  if (!["prepare", "check", "attest", "finalize", "ready"].includes(stage)) throw new Error("Expected prepare, check, attest, finalize, or ready");
  if (!/^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9_][A-Za-z0-9_.-]*$/.test(repo)) throw new Error("Invalid repository");
  const parsed = parseTag(tag);
  if (!parsed.ok) throw new Error(parsed.reason);
  const releaseVersion = parsed.version;
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error("Expected the full preflight commit SHA");
  if (stage !== "prepare" && stage !== "ready" && !/^[1-9]\d*$/.test(releaseId ?? "")) throw new Error("Expected the prepared release ID");
  if (stage === "finalize" && (!notes || Buffer.byteLength(notes) > 125_000)) throw new Error("Missing or oversized release notes");
  if (stage === "finalize" && (typeof downloadDir !== "string" || downloadDir === "")) throw new Error("finalize needs RELEASE_DOWNLOAD_DIR to open the bundles");
  if (stage === "finalize" && (!Array.isArray(binaries) || binaries.length === 0)) throw new Error("finalize needs the declared [[bin]] names to look for");
  if (stage === "attest" && ![sboms?.spdx, sboms?.cargo].every(text => typeof text === "string" && text.length > 0 && Buffer.byteLength(text) <= 32 * 1024 * 1024)) {
    throw new Error("Missing, empty or oversized SBOM");
  }
  const base = `repos/${repo}`;
  /** @param {string} endpoint @param {string} [method] @param {Record<string, unknown>} [body] @param {boolean} [allowMissing] */
  function api(endpoint, method = "GET", body, allowMissing = false) {
    const result = run("gh", ["api", "--include", "--method", method, `${base}/${endpoint}`,
      ...(body ? ["--input", "-"] : [])], body ? JSON.stringify(body) : undefined);
    return response(result, method, endpoint, allowMissing);
  }
  /**
   * Uploads one asset through the draft's own `upload_url`, which must name
   * this repository's prepared release on GitHub's upload host.
   * @param {Record<string, unknown>} release @param {string} name @param {string} content @param {string} contentType
   */
  function upload(release, name, content, contentType) {
    const template = typeof release.upload_url === "string" ? release.upload_url : "";
    const prefix = `https://uploads.github.com/${base}/releases/${releaseId}/assets`;
    if (template !== `${prefix}{?name,label}`) throw new Error("Draft upload URL is missing or names another release");
    const result = run("gh", ["api", "--include", "--method", "POST", `${prefix}?name=${encodeURIComponent(name)}`,
      "-H", `Content-Type: ${contentType}`, "--input", "-"], content);
    return record(response(result, "POST", `upload ${name}`, false));
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
   * Prepare's 5-minute budget cannot wait out a 30-minute CI matrix. An in-flight
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
  }
  if (stage === "ready") {
    checkCheckout();
    for (const workflow of ["ci.yml", "coverage.yml"]) requireSuccessfulPushRun(workflow);
    return { release_id: "", commit, tag, stage };
  }
  const tagIdentity = checkTag();
  /** @param {Record<string, unknown> | null} release */
  function assertMutableDraft(release) {
    if (!release || release.draft !== true || release.prerelease !== false || release.immutable === true || release.published_at !== null) throw new Error("Release is not a mutable unpublished draft");
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
   * Every asset's id, name and GitHub-computed SHA-256, sorted by name.
   * @param {Record<string, unknown>} release
   */
  function assetDigests(release) {
    if (!Array.isArray(release.assets)) throw new Error("Missing asset metadata");
    const assets = release.assets.map(asset => {
      const metadata = record(asset);
      if (typeof metadata.id !== "number" || !Number.isSafeInteger(metadata.id) || metadata.id <= 0 ||
          typeof metadata.digest !== "string" || !/^sha256:[a-f0-9]{64}$/i.test(metadata.digest)) throw new Error("Release asset ID or SHA-256 metadata is missing or invalid");
      return {id: metadata.id, name: String(metadata.name), size: metadata.size, state: metadata.state, digest: metadata.digest.toLowerCase()};
    }).sort((a, b) => a.name.localeCompare(b.name));
    if (new Set(assets.map(asset => asset.id)).size !== assets.length) throw new Error("Duplicate release asset IDs");
    return assets;
  }
  /**
   * Downloads each bundled installer by asset ID, proves the bytes are the ones
   * GitHub holds (size and SHA-256 from the draft's metadata) before opening
   * them, requires every declared binary inside, and proves the files did not
   * change while being read. Returns the coverage statement.
   * @param {Record<string, unknown>} release
   */
  function checkBundleContents(release) {
    const byName = new Map(assetDigests(release).map(asset => [asset.name, asset]));
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
      for (const {spec, id, size, digest} of wanted) {
        const dest = path.join(work, spec.asset);
        fetchAsset({repo, asset: {id, name: spec.asset, size, digest}, dest});
        const got = statSync(dest).size;
        if (got !== size) throw new Error(`Bundle ${spec.asset} downloaded ${got} bytes, expected ${size}`);
        if (`sha256:${sha256File(dest)}` !== digest) throw new Error(`Bundle ${spec.asset} does not match GitHub's SHA-256`);
        files[spec.asset] = dest;
        digests[spec.asset] = digest;
      }
      const result = inspect({version: releaseVersion, binaries: binaries ?? [], files, workDir: work});
      if (!result.ok) throw new Error(`Bundle contents are wrong: ${result.violations.join("; ")} (${result.coverage})`);
      for (const [name, file] of Object.entries(files)) {
        if (`sha256:${sha256File(file)}` !== digests[name]) throw new Error(`Bundle ${name} changed while it was being inspected`);
      }
      return result.coverage;
    } finally {
      rmSync(work, {recursive: true, force: true});
    }
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
    for (const workflow of ["ci.yml", "coverage.yml"]) requireSuccessfulPushRun(workflow);
    release = findExistingDraft();
    if (!release) {
      // Do not automatically retry a POST whose outcome is unknown. A rerun
      // reads the existing draft before deciding whether creation is necessary.
      release = record(api("releases", "POST", { tag_name: tag, target_commitish: commit, name: `GitPulse ${tag}`, draft: true, prerelease: false, body: "Draft — platform builds and verification are pending." }));
    } else {
      // Identity first. `/releases/tags/{tag}` can still return a published
      // release; wiping its installers would delete the last successful set.
      release = assertMutableDraft(release);
      assertDraftTag(release);
      // The matrix always rebuilds. Leftover names — especially the Windows
      // NSIS/MSI pair from a cancelled attempt — make GitHub 422
      // already_exists. tauri-action retries that three times and then fails
      // the Windows leg with every other platform already uploaded. Wipe the
      // previous attempt's installers before the matrix runs.
      const assets = Array.isArray(release.assets) ? release.assets : [];
      for (const raw of assets) {
        const metadata = record(raw);
        if (typeof metadata.id !== "number" || !Number.isSafeInteger(metadata.id) || metadata.id <= 0) {
          throw new Error("Release asset ID is missing or invalid");
        }
        api(`releases/assets/${metadata.id}`, "DELETE", undefined, true);
      }
      if (assets.length > 0) {
        const cleared = record(api(`releases/${release.id}`));
        if (Array.isArray(cleared.assets) && cleared.assets.length > 0) {
          throw new Error("Draft still has installer assets after wipe");
        }
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
    // A rerun of this stage finds its own earlier uploads. Replace those, and
    // only those: an installer is never deleted here.
    const leftovers = assetDigests(release).filter(asset => supplements.has(asset.name));
    for (const asset of leftovers) api(`releases/assets/${asset.id}`, "DELETE");
    if (leftovers.length > 0) release = checkDraft(record(api(`releases/${releaseId}`)));
    const installers = assetDigests(release);
    const wanted = expectedInstallerNames(version);
    const present = installers.map(asset => asset.name);
    const missing = wanted.filter(name => !present.includes(name));
    const unexpected = present.filter(name => !wanted.includes(name));
    if (missing.length > 0 || unexpected.length > 0) {
      throw new Error(`Installer set is not exact (missing: ${missing.join(", ") || "none"}; unexpected: ${unexpected.join(", ") || "none"})`);
    }
    /** @param {string} text */
    const sha256 = text => createHash("sha256").update(text, "utf8").digest("hex");
    const local = new Map([[names.spdx, sha256(sboms?.spdx ?? "")], [names.cargo, sha256(sboms?.cargo ?? "")]]);
    // GitHub's digest is computed over the stored bytes, which are exactly
    // what a user downloads; the SBOM digests are checked against it below.
    const sums = [...installers.map(asset => [asset.digest.slice("sha256:".length), asset.name]), ...[...local].map(([name, hash]) => [hash, name])]
      .sort((a, b) => a[1].localeCompare(b[1]))
      .map(([hash, name]) => `${hash}  ${name}\n`)
      .join("");
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
    const tagAtHead = run("git", ["tag", "--points-at", "HEAD"]).stdout.trim().split(/\s+/).find(t => /^v\d+\.\d+\.\d+$/.test(t));
    if (tagAtHead) {
      tag = tagAtHead;
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
