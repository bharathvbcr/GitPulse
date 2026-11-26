import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import { bundleSpecs, expectedAssetNames, expectedInstallerNames, supplementNames } from "./check-release-assets.mjs";
import { compareCodePoints, fetchReleaseAsset, formatChecksumsManifest, isPermanentDownloadFailure, isTransientUploadError, MAX_API_PAGES, parseChecksumsManifest, REQUIRED_WORKFLOW_JOBS, runReleaseStage, runCommand, main, resolveReleaseContext, validateSbomDocuments, verifyChecksumsManifest, type Runner } from "./release-state.mjs";

const commit = "a".repeat(40);
const tag = "v1.2.3";
const options = { stage: "prepare", repo: "owner/repo", tag, commit };
const checksumsAssetName = supplementNames("1.2.3").checksums;
const rawStubBytes = (name: string) => Buffer.from(`bytes:${name}`);
const rawStubDigest = (name: string) => `sha256:${createHash("sha256").update(rawStubBytes(name)).digest("hex")}`;
const canonicalSumsBytes = Buffer.from(
  formatChecksumsManifest(
    expectedAssetNames("1.2.3")
      .filter((name) => name !== checksumsAssetName)
      .map((name) => ({ name, digest: rawStubDigest(name) })),
  ),
  "utf8",
);
/** The bytes the stub download writes for an asset; the draft's size and digest describe exactly these. */
const stubBytes = (name: string) => (name === checksumsAssetName ? canonicalSumsBytes : rawStubBytes(name));
const stubDigest = (name: string) => `sha256:${createHash("sha256").update(stubBytes(name)).digest("hex")}`;
function draft() {
  return {id: 42, tag_name: tag, name: `Waylode ${tag}`, target_commitish: commit, draft: true, prerelease: false,
    immutable: false, published_at: null, body: "pending",
    upload_url: "https://uploads.github.com/repos/owner/repo/releases/42/assets{?name,label}",
    assets: expectedAssetNames("1.2.3")
      .map((name, index) => ({id: index + 1, name, size: stubBytes(name).length, state: "uploaded", digest: stubDigest(name)}))};
}
function defaultJobsFor(workflow: string) {
  return (REQUIRED_WORKFLOW_JOBS[workflow] ?? []).map((name, index) => ({
    id: 5000 + index,
    name,
    head_sha: commit,
    status: "completed",
    conclusion: "success",
  }));
}
function fixture(change: {
  release?: Record<string, unknown> | null;
  ci?: Record<string, unknown>;
  ciJobs?: Record<string, { jobs: unknown[]; total_count?: number }>;
  ciJobPages?: Record<string, Array<{ jobs: unknown[]; total_count?: number }>>;
  remote?: string;
  local?: string;
  fail?: string;
  corruptNotes?: boolean;
  changeAssets?: boolean;
  publishAfterPatch?: boolean;
  apiStatus?: number;
  uncertainPost?: boolean;
  downloadDuringFinalize?: boolean;
  crlfNotes?: boolean;
  list?: unknown[];
  releasePages?: unknown[][];
  wipeResidualAssets?: unknown[][];
  corruptUpload?: boolean;
  flakyUploads?: number;
  uploadStatus?: number;
} = {}) {
  let release = change.release === undefined ? draft() : change.release;
  const calls: string[][] = [];
  const patches: string[] = [];
  const uploadedBytes = new Map<string, Buffer>();
  const residualQueue = [...(change.wipeResidualAssets ?? [])];
  let patched = false;
  let remainingUploadFailures = change.flakyUploads ?? 0;
  const runIdToWorkflow = new Map<number, string>([[100, "ci.yml"], [101, "coverage.yml"]]);
  const run: Runner = (program, args, input) => {
    calls.push([program, ...args]);
    if (program === "git") return {status: 0, failed: false, stdout: args[0] === "rev-parse" ? change.local ?? commit : change.remote ?? `${commit}\trefs/tags/${tag}\n`};
    const endpoint = args[4];
    if (change.fail && endpoint.includes(change.fail)) return {status: null, failed: true, stdout: ""};
    const respond = (value: unknown, status = 200) => ({status: status >= 400 ? 1 : 0, failed: false, stdout: `HTTP/2.0 ${status} Status\nContent-Type: application/json\r\n\r\n${JSON.stringify(value)}`});
    if (endpoint.includes("releases/tags/")) {
      if (change.apiStatus) return respond({message: "unavailable"}, change.apiStatus);
      const wanted = endpoint.slice(endpoint.indexOf("releases/tags/") + "releases/tags/".length);
      if (release && release.draft !== true && release.tag_name === wanted) return respond(release);
      return respond({message: "Not Found"}, 404);
    }
    if (endpoint.includes("actions/workflows/")) {
      const wfMatch = /actions\/workflows\/([^/]+)\/runs/.exec(endpoint);
      const wf = wfMatch?.[1] ?? "ci.yml";
      const runId = wf === "coverage.yml" ? 101 : 100;
      const runRecord = change.ci ? {id: runId, ...change.ci} : {id: runId, head_sha: commit, event: "push", status: "completed", conclusion: "success"};
      return respond({workflow_runs: [runRecord]});
    }
    if (endpoint.includes("actions/runs/") && endpoint.includes("/jobs")) {
      const runMatch = /actions\/runs\/(\d+)\/jobs/.exec(endpoint);
      const wf = runIdToWorkflow.get(Number(runMatch?.[1] ?? 0)) ?? "ci.yml";
      const pageMatch = /[?&]page=(\d+)/.exec(endpoint);
      const pageIndex = Math.max(0, Number(pageMatch?.[1] ?? 1) - 1);
      const paged = change.ciJobPages?.[wf];
      if (paged) {
        const page = paged[pageIndex] ?? { jobs: [], total_count: paged[0]?.total_count };
        return respond({ total_count: page.total_count, jobs: page.jobs });
      }
      const override = change.ciJobs?.[wf];
      const jobs = override ? override.jobs : defaultJobsFor(wf);
      const total_count = override?.total_count ?? jobs.length;
      return respond({total_count, jobs});
    }
    if (args[3] === "DELETE") {
      const assetMatch = /releases\/assets\/(\d+)$/.exec(String(args[4] ?? ""));
      if (assetMatch && release && Array.isArray(release.assets)) {
        const id = Number(assetMatch[1]);
        release = {...release, assets: release.assets.filter(asset => asset.id !== id)};
      }
      return {status: 0, failed: false, stdout: "HTTP/2.0 204 No Content\r\n\r\n"};
    }
    if (args[3] === "POST" && endpoint.startsWith("https://uploads.github.com/")) {
      const name = decodeURIComponent(endpoint.slice(endpoint.indexOf("?name=") + "?name=".length));
      if (change.uploadStatus) return respond({message: "upload error"}, change.uploadStatus);
      if (remainingUploadFailures > 0) {
        remainingUploadFailures -= 1;
        // Simulate GitHub leaving a partial state="starter" asset with null digest after a 502.
        const assets = release && Array.isArray(release.assets) ? release.assets : [];
        release = {...release, assets: [...assets, {id: 1900 + assets.length, name, size: 0, state: "starter", digest: null}]};
        return respond({message: "Bad Gateway"}, 502);
      }
      const payload = input ?? "";
      const hash = createHash("sha256").update(change.corruptUpload ? `${payload}!` : payload, "utf8").digest("hex");
      const assets = release && Array.isArray(release.assets) ? release.assets : [];
      const asset = {id: 1000 + assets.length, name, size: Buffer.byteLength(payload), state: "uploaded", digest: `sha256:${hash}`};
      release = {...release, assets: [...assets, asset]};
      uploadedBytes.set(name, Buffer.from(payload, "utf8"));
      return respond(asset, 201);
    }
    if (args[3] === "POST") { release = {...draft(), ...JSON.parse(input ?? "{}")}; return change.uncertainPost ? {status: null, failed: true, stdout: ""} : respond(release, 201); }
    if (args[3] === "PATCH") {
      patched = true;
      patches.push(input ?? "");
      release = {...release, ...JSON.parse(input ?? "{}")};
      if (change.corruptNotes && release) release.body = "damaged";
      if (change.crlfNotes && release && typeof release.body === "string") release.body = release.body.replace(/\n/g, "\r\n");
      if (change.publishAfterPatch && release) release.draft = false;
      return respond(release);
    }
    if (String(args[4] ?? "").includes("releases?per_page=")) {
      if (change.releasePages) {
        const pageMatch = /[?&]page=(\d+)/.exec(String(args[4] ?? ""));
        const pageIndex = Math.max(0, Number(pageMatch?.[1] ?? 1) - 1);
        return respond(change.releasePages[pageIndex] ?? []);
      }
      return respond(change.list ?? (release ? [release] : []));
    }
    if (release && Array.isArray(release.assets) && release.assets.length === 0 && residualQueue.length > 0 && /\/releases\/\d+$/.test(endpoint)) {
      release = { ...release, assets: residualQueue.shift() ?? [] };
    }
    if (patched && change.changeAssets && release) release.assets = [];
    if (patched && change.downloadDuringFinalize && release) release.assets = draft().assets.map(asset => ({...asset, download_count: 1})).reverse();
    return release ? respond(release) : respond({message: "Not Found"}, 404);
  };
  return {run, calls, patches, uploadedBytes};
}

const binaries = ["waylode", "waylode-hook", "waylode-mcp", "waylode-ptyd", "wayloded"];
const scratch: string[] = [];
afterAll(() => { for (const dir of scratch) rmSync(dir, {recursive: true, force: true}); });

/**
 * The download and the inspection, stubbed: the draft's sizes and digests
 * describe `stubBytes`, so an honest fetch passes and each fault changes
 * exactly one thing. `fetched` and `inspected` record what ran.
 */
type Verdict = {ok: boolean; invalid: boolean; violations: string[]; opened: string[]; coverage: string};
function bundleDeps(fault: {corrupt?: string; short?: string; fail?: string; order?: string[]; swapDuringInspect?: boolean; verdict?: Verdict; bytesOverride?: Map<string, Buffer>} = {}) {
  const downloadDir = path.join(mkdtempSync(path.join(tmpdir(), "waylode-release-state-")), "bundles");
  scratch.push(path.dirname(downloadDir));
  const fetched: string[] = [];
  const inspected: {binaries: string[]; files: Record<string, string | undefined>}[] = [];
  return {
    downloadDir, binaries, fetched, inspected,
    fetchAsset: ({asset, dest}: {asset: {name: string}; dest: string}) => {
      fetched.push(asset.name);
      if (fault.fail === asset.name) throw new Error("download refused");
      const bytes = fault.bytesOverride?.get(asset.name) ?? stubBytes(asset.name);
      writeFileSync(dest, fault.corrupt === asset.name ? Buffer.from(bytes.toString().toUpperCase()) : fault.short === asset.name ? bytes.subarray(1) : bytes);
    },
    inspect: (request: {binaries: string[]; files: Record<string, string | undefined>}) => {
      inspected.push(request);
      fault.order?.push("inspect");
      if (fault.swapDuringInspect) writeFileSync(String(Object.values(request.files)[0]), "swapped");
      return fault.verdict ?? {ok: true, invalid: false, violations: [], opened: Object.keys(request.files), coverage: "opened 7 of 12 installers; AppImage, dmg and rpm are not opened"};
    },
  };
}

describe("remote release lifecycle", () => {
  it("creates a draft only after successful CI and coverage for the pinned commit", () => {
    const {run, calls} = fixture({release: null});
    expect(runReleaseStage(options, run).release_id).toBe("42");
    expect(calls.filter(call => call.includes("POST"))).toHaveLength(1);
    expect(calls.filter(call => call.some(arg => arg.includes("actions/workflows/")))).toHaveLength(2);
  });
  it("drafts a beta tag as a GitHub pre-release and refuses a stable draft left for it", () => {
    // Publishing the draft must never make a beta the repository's Latest release.
    const betaTag = "v0.2.0-beta.1";
    const betaRemote = `${commit}\trefs/tags/${betaTag}\n`;
    const {run} = fixture({release: null, remote: betaRemote});
    const bodies: string[] = [];
    const spy: Runner = (program, args, input) => {
      if (args[3] === "POST" && !String(args[4] ?? "").startsWith("https://uploads.github.com/")) bodies.push(input ?? "");
      return run(program, args, input);
    };
    expect(runReleaseStage({...options, tag: betaTag}, spy).release_id).toBe("42");
    expect(bodies).toHaveLength(1);
    expect(JSON.parse(bodies[0])).toMatchObject({tag_name: betaTag, draft: true, prerelease: true});

    const stable = fixture({release: {...draft(), tag_name: betaTag, name: `GitPulse ${betaTag}`, prerelease: false}, remote: betaRemote});
    expect(() => runReleaseStage({...options, tag: betaTag}, stable.run)).toThrow(/mutable unpublished draft/);
    expect(stable.calls.some(call => call.includes("POST") || call.includes("PATCH") || call.includes("DELETE"))).toBe(false);
  });
  it("resumes an existing matching draft without duplicate creation", () => {
    const {run, calls} = fixture();
    expect(runReleaseStage(options, run).release_id).toBe("42");
    expect(calls.some(call => call.includes("POST"))).toBe(false);
  });
  it("resumes a draft GitHub detached from the git tag instead of creating a second one", () => {
    const untagged = {...draft(), tag_name: "untagged-f782e62dab083869a408"};
    const {run, calls} = fixture({release: untagged, list: [untagged]});
    expect(runReleaseStage(options, run).release_id).toBe("42");
    expect(calls.filter(call => call.includes("POST"))).toHaveLength(0);
  });
  it("resumes a still-tagged draft that /releases/tags/ cannot see even when the display name is missing", () => {
    const nameless: Record<string, unknown> = {...draft()};
    delete nameless.name;

    const {run, calls} = fixture({release: nameless, list: [nameless]});
    expect(runReleaseStage(options, run).release_id).toBe("42");
    expect(calls.filter(call => call.includes("POST"))).toHaveLength(0);
  });
  it("ignores a published untagged release when looking for this draft", () => {
    const published = {...draft(), draft: false, published_at: "2026-09-09T06:46:15Z", tag_name: "untagged-2db6b2cc5c3ed181b405"};
    const {run, calls} = fixture({apiStatus: 404, release: null, list: [published]});
    expect(runReleaseStage(options, run).release_id).toBe("42");
    expect(calls.filter(call => call.includes("POST"))).toHaveLength(1);
  });
  it("paginates releases beyond 100 entries and refuses when the page cap is exceeded rather than posting a duplicate", () => {
    const page1 = Array.from({length: 100}, (_, index) => ({...draft(), id: 1000 + index, name: `Other ${index}`, tag_name: `v0.0.${index}`}));
    const page2Match = {...draft(), id: 42, tag_name: "untagged-deadbeef02", assets: []};
    // Finds a matching draft on page 2 without creating a duplicate.
    const found = fixture({apiStatus: 404, release: page2Match, releasePages: [page1, [page2Match]]});
    expect(runReleaseStage(options, found.run).release_id).toBe("42");
    expect(found.calls.some(call => call.includes("POST"))).toBe(false);
    expect(found.calls.some(call => call.some(arg => arg.includes("releases?per_page=100&page=2")))).toBe(true);

    // Detects duplicate matching drafts even when they appear on different pages.
    const dupPage1 = [{...draft(), id: 42, name: `GitPulse ${tag}`, tag_name: "untagged-a1"}, ...page1.slice(1)];
    const dupPage2 = [{...draft(), id: 43, name: `GitPulse ${tag}`, tag_name: "untagged-a2"}];
    const dups = fixture({apiStatus: 404, release: null, releasePages: [dupPage1, dupPage2]});
    expect(() => runReleaseStage(options, dups.run)).toThrow(/Multiple matching drafts/);
    expect(dups.calls.some(call => call.includes("POST"))).toBe(false);

    // Deduplicates a release shifted across page boundaries instead of treating it as two drafts.
    const shifted = fixture({apiStatus: 404, release: page2Match, releasePages: [[page2Match, ...page1.slice(1)], [page2Match]]});
    expect(runReleaseStage(options, shifted.run).release_id).toBe("42");

    // Refuses when all MAX_API_PAGES pages are full rather than posting a duplicate.
    const capped = fixture({apiStatus: 404, release: null, list: page1});
    expect(() => runReleaseStage(options, capped.run)).toThrow(new RegExp(`capped at ${MAX_API_PAGES * 100}`));
    expect(capped.calls.some(call => call.includes("POST"))).toBe(false);
  });
  it("refuses multiple drafts that share this release name", () => {
    const {run, calls} = fixture({
      list: [draft(), {...draft(), id: 43, tag_name: "untagged-abc"}],
    });
    expect(() => runReleaseStage(options, run)).toThrow(/Multiple matching drafts/);
    expect(calls.some(call => call.includes("POST"))).toBe(false);
  });
  it("recovers an uncertain create on rerun without issuing a second POST", () => {
    const {run, calls} = fixture({release: null, uncertainPost: true});
    expect(() => runReleaseStage(options, run)).toThrow(/incomplete response/);
    expect(runReleaseStage(options, run).release_id).toBe("42");
    expect(calls.filter(call => call.includes("POST"))).toHaveLength(1);
  });
  it.each([401, 403, 429, 500, 502])("refuses HTTP %s without creating a release", apiStatus => {
    const {run, calls} = fixture({apiStatus});
    expect(() => runReleaseStage(options, run)).toThrow(`HTTP ${apiStatus}`);
    expect(calls.some(call => call.includes("POST"))).toBe(false);
  });
  it("bounds real child output and reports launch failure explicitly", () => {
    const oversized = runCommand(process.execPath, ["-e", "process.stdout.write('x'.repeat(5*1024*1024))"]);
    expect(oversized.failed).toBe(true);
    expect(runCommand("gitpulse-no-such-release-program", []).failed).toBe(true);
  });
  it("resolves an annotated tag while keeping the peeled commit", () => {
    const {run} = fixture({remote: `${"c".repeat(40)}\trefs/tags/${tag}\n${commit}\trefs/tags/${tag}^{}\n`});
    expect(runReleaseStage(options, run).commit).toBe(commit);
  });
  it("accepts GitHub echoing the annotated tag object SHA as target_commitish", () => {
    const tagObject = "c".repeat(40);
    const {run} = fixture({
      remote: `${tagObject}\trefs/tags/${tag}\n${commit}\trefs/tags/${tag}^{}\n`,
      release: {...draft(), target_commitish: tagObject},
    });
    expect(runReleaseStage(options, run).release_id).toBe("42");
  });
  it.each([
    {draft: false}, {draft: undefined}, {published_at: "2026-09-08"}, {immutable: true}, {prerelease: true},
    {id: 0}, {id: 9007199254740992},
  ])("refuses unsafe existing release metadata %j", change => {
    const {run, calls} = fixture({release: {...draft(), ...change}});
    expect(() => runReleaseStage(options, run)).toThrow();
    expect(calls.some(call => call.includes("POST") || call.includes("PATCH") || call.includes("DELETE"))).toBe(false);
  });
  it("clears leftover installer assets before a rebuild so Windows cannot 422 on a name that already exists", () => {
    const leftover = {
      ...draft(),
      assets: [
        {id: 11, name: "Waylode_1.2.3_x64-setup.exe", size: 10, state: "uploaded", digest: `sha256:${"b".repeat(64)}`},
        {id: 12, name: "Waylode_1.2.3_x64_en-US.msi", size: 10, state: "uploaded", digest: `sha256:${"b".repeat(64)}`},
      ],
    };
    const {run, calls} = fixture({release: leftover, list: [leftover]});
    expect(runReleaseStage(options, run).release_id).toBe("42");
    const deleted = calls.filter(call => call.includes("DELETE")).map(call => call.find(arg => String(arg).includes("releases/assets/")) ?? "");
    expect(deleted.some(arg => arg.endsWith("/releases/assets/11"))).toBe(true);
    expect(deleted.some(arg => arg.endsWith("/releases/assets/12"))).toBe(true);
    expect(calls.some(call => call.includes("POST"))).toBe(false);
  });
  it("retargets a leftover draft onto this commit instead of refusing the SHA GitHub still has", () => {
    const stale = {...draft(), target_commitish: "d".repeat(40), assets: []};
    const {run, patches, calls} = fixture({release: stale, list: [stale]});
    expect(runReleaseStage(options, run).release_id).toBe("42");
    expect(calls.filter(call => call.includes("POST"))).toHaveLength(0);
    expect(patches.some(body => JSON.parse(body).target_commitish === commit && JSON.parse(body).tag_name === tag)).toBe(true);
  });
  it("refuses a leftover installer that has no asset id instead of skipping it", () => {
    const leftover = {
      ...draft(),
      assets: [{name: "Waylode_1.2.3_x64-setup.exe", size: 10, state: "uploaded", digest: `sha256:${"b".repeat(64)}`}],
    };
    const {run, calls} = fixture({release: leftover, list: [leftover]});
    expect(() => runReleaseStage(options, run)).toThrow(/asset ID/);
    expect(calls.some(call => call.includes("DELETE"))).toBe(false);
  });
  it("does not delete installers during the pre-upload identity check", () => {
    const {run, calls} = fixture();
    expect(runReleaseStage({...options, stage: "check", releaseId: "42"}, run).stage).toBe("check");
    expect(calls.some(call => call.includes("DELETE"))).toBe(false);
  });
  it("refuses a different version tag on check rather than renaming it mid-upload", () => {
    const {run, calls} = fixture({release: {...draft(), tag_name: "v0.0.8"}});
    expect(() => runReleaseStage({...options, stage: "check", releaseId: "42"}, run)).toThrow(/Draft tag/);
    expect(calls.some(call => call.includes("POST") || call.includes("PATCH") || call.includes("DELETE"))).toBe(false);
  });
  it.each(["main", tag, `refs/tags/${tag}`, commit.slice(0, 7)])("accepts GitHub echoing ref name %s after the tag is associated", commitish => {
    const {run, calls} = fixture({release: {...draft(), target_commitish: commitish}});
    expect(runReleaseStage(options, run).release_id).toBe("42");
    expect(calls.some(call => call.includes("POST") || call.includes("PATCH"))).toBe(false);
  });
  it("writes the intended tag name back when prepare resumes an untagged draft", () => {
    const {run, calls, patches} = fixture({release: {...draft(), tag_name: "untagged-f782e62dab083869a408"}});
    expect(runReleaseStage(options, run).release_id).toBe("42");
    expect(calls.some(call => call.includes("POST"))).toBe(false);
    expect(patches.some(body => JSON.parse(body).tag_name === tag && JSON.parse(body).target_commitish === commit)).toBe(true);
  });
  it.each(["untagged", "untagged-", "untagged-not-hex", "latest", "v0.0.8"])("refuses a tag name that is not this release or GitHub's untagged hex form: %s", tag_name => {
    const {run, calls} = fixture({release: {...draft(), tag_name}});
    expect(() => runReleaseStage(options, run)).toThrow(/Draft tag/);
    expect(calls.some(call => call.includes("POST") || call.includes("PATCH"))).toBe(false);
  });
  it("accepts an annotated tag object's SHA as target_commitish", () => {
    const object = "c".repeat(40);
    const {run} = fixture({
      remote: `${object}\trefs/tags/${tag}\n${commit}\trefs/tags/${tag}^{}\n`,
      release: {...draft(), target_commitish: object},
    });
    expect(runReleaseStage(options, run).release_id).toBe("42");
  });
  it.each(["failure", "cancelled", "skipped", null])("refuses CI conclusion %s", conclusion => {
    const {run, calls} = fixture({ci: {head_sha: commit, event: "push", status: "completed", conclusion}});
    expect(() => runReleaseStage(options, run)).toThrow(/concluded/);
    expect(calls.some(call => call.some(arg => arg.includes("/releases")))).toBe(false);
  });
  it.each(["queued", "in_progress"])("refuses an in-flight CI run as %s rather than as missing", status => {
    const {run, calls} = fixture({ci: {head_sha: commit, event: "push", status, conclusion: null}});
    expect(() => runReleaseStage(options, run)).toThrow(/still/);
    expect(() => runReleaseStage(options, run)).toThrow(/wait for it to succeed before tagging/);
    expect(calls.some(call => call.some(arg => arg.includes("/releases")))).toBe(false);
  });
  it("refuses a commit CI has not started for", () => {
    const {run, calls} = fixture();
    const empty: Runner = (program, args, input) => {
      if (program === "gh" && String(args[4] ?? "").includes("actions/workflows/")) {
        return {status: 0, failed: false, stdout: "HTTP/2.0 200 Status\nContent-Type: application/json\r\n\r\n{\"workflow_runs\":[]}"};
      }
      return run(program, args, input);
    };
    expect(() => runReleaseStage(options, empty)).toThrow(/has no push run/);
    expect(calls.some(call => call.some(arg => arg.includes("/releases")))).toBe(false);
  });
  it("refuses a CI run missing a required shard or completeness gate even when run conclusion is success", () => {
    const missingShard = defaultJobsFor("ci.yml").filter(job => job.name !== "Rust Tests (Windows, shard 4/4)");
    const {run: runMissing, calls: callsMissing} = fixture({ciJobs: {"ci.yml": {jobs: missingShard}}});
    expect(() => runReleaseStage(options, runMissing)).toThrow(/missing required job "Rust Tests \(Windows, shard 4\/4\)"/);
    expect(callsMissing.some(call => call.some(arg => arg.includes("/releases")))).toBe(false);

    const skippedComplete = defaultJobsFor("ci.yml").map(job =>
      job.name === "Rust Tests (every OS shard ran)" ? {...job, conclusion: "skipped"} : job
    );
    const {run: runSkipped, calls: callsSkipped} = fixture({ciJobs: {"ci.yml": {jobs: skippedComplete}}});
    expect(() => runReleaseStage({...options, stage: "ready"}, runSkipped)).toThrow(/required job "Rust Tests \(every OS shard ran\)" did not succeed/);
    expect(callsSkipped.some(call => call.some(arg => arg.includes("/releases")))).toBe(false);
  });
  it("wipes leftover assets across multiple passes when inline asset lists span pages and fails closed if assets persist", () => {
    const pass1 = {
      ...draft(),
      assets: [{id: 11, name: "Waylode_1.2.3_x64-setup.exe", size: 10, state: "uploaded", digest: `sha256:${"b".repeat(64)}`}],
    };
    const pass2 = [{id: 12, name: "Waylode_1.2.3_x64_en-US.msi", size: 10, state: "uploaded", digest: `sha256:${"c".repeat(64)}`}];
    const multi = fixture({release: pass1, list: [pass1], wipeResidualAssets: [pass2]});
    expect(runReleaseStage(options, multi.run).release_id).toBe("42");
    const deleted = multi.calls.filter(call => call.includes("DELETE")).map(call => call[5]);
    expect(deleted).toEqual(["repos/owner/repo/releases/assets/11", "repos/owner/repo/releases/assets/12"]);

    const stuck = fixture({release: pass1, list: [pass1], wipeResidualAssets: [pass2, pass2, pass2, pass2, pass2]});
    expect(() => runReleaseStage(options, stuck.run)).toThrow(/Draft still has installer assets after wipe/);
  });

  it("refuses a truncated CI job list, missing head_sha, or a job executed against another commit, and paginates >100 jobs", () => {
    const truncated = fixture({ciJobs: {"ci.yml": {jobs: defaultJobsFor("ci.yml"), total_count: 105}}});
    expect(() => runReleaseStage(options, truncated.run)).toThrow(/job list was truncated/);

    const missingSha = defaultJobsFor("ci.yml").map(job =>
      job.name === "Frontend (ubuntu-latest)" ? {...job, head_sha: undefined} : job
    );
    expect(() => runReleaseStage(options, fixture({ciJobs: {"ci.yml": {jobs: missingSha}}}).run)).toThrow(/ran against/);

    const wrongSha = defaultJobsFor("ci.yml").map(job =>
      job.name === "Frontend (ubuntu-latest)" ? {...job, head_sha: "b".repeat(40)} : job
    );
    const {run} = fixture({ciJobs: {"ci.yml": {jobs: wrongSha}}});
    expect(() => runReleaseStage(options, run)).toThrow(/ran against/);

    // Paginate >100 jobs across page 1 + page 2: required job on page 2 passes when green, fails when red.
    const required = defaultJobsFor("ci.yml");
    const filler = Array.from({length: 85}, (_, i) => ({
      id: 8000 + i,
      name: `Extra Matrix Job ${i}`,
      head_sha: commit,
      status: "completed",
      conclusion: "success",
    }));
    const page1 = [...required.slice(0, 15), ...filler]; // 100 jobs
    const page2 = required.slice(15); // remaining 7 required jobs -> total 107
    const pagedOk = fixture({
      release: null,
      ciJobPages: {"ci.yml": [{jobs: page1, total_count: 107}, {jobs: page2, total_count: 107}]},
    });
    expect(runReleaseStage({...options, stage: "ready"}, pagedOk.run).stage).toBe("ready");
    expect(pagedOk.calls.some(call => call.some(arg => arg.includes("actions/runs/100/jobs?filter=latest&per_page=100&page=2")))).toBe(true);

    const page2Failed = page2.map((job, idx) => (idx === 0 ? {...job, conclusion: "failure"} : job));
    const pagedFail = fixture({
      release: null,
      ciJobPages: {"ci.yml": [{jobs: page1, total_count: 107}, {jobs: page2Failed, total_count: 107}]},
    });
    expect(() => runReleaseStage({...options, stage: "ready"}, pagedFail.run)).toThrow(/did not succeed/);

    // Refuse when total_count mutates mid-pagination.
    const mutated = fixture({
      release: null,
      ciJobPages: {"ci.yml": [{jobs: page1, total_count: 107}, {jobs: page2, total_count: 108}]},
    });
    expect(() => runReleaseStage({...options, stage: "ready"}, mutated.run)).toThrow(/changed during pagination/);

    // Refuse when pagination exceeds MAX_API_PAGES.
    const infinitePages = Array.from({length: MAX_API_PAGES + 1}, (_, p) => ({
      jobs: Array.from({length: 100}, (_, i) => ({id: 10_000 + p * 100 + i, name: `Job ${p}-${i}`, head_sha: commit, status: "completed", conclusion: "success"})),
      total_count: (MAX_API_PAGES + 1) * 100,
    }));
    const overflow = fixture({release: null, ciJobPages: {"ci.yml": infinitePages}});
    expect(() => runReleaseStage({...options, stage: "ready"}, overflow.run)).toThrow(/job list was truncated/);
  });
  it("ready checks CI without requiring the tag to already point here or mutating a draft", () => {
    const {run, calls} = fixture();
    expect(runReleaseStage({...options, stage: "ready"}, run)).toEqual({release_id: "", commit, tag, stage: "ready"});
    expect(calls.some(call => call[0] === "git" && call.includes("ls-remote"))).toBe(false);
    expect(calls.some(call => call.includes("POST") || call.includes("PATCH") || call.includes("DELETE"))).toBe(false);
    expect(calls.filter(call => call.some(arg => arg.includes("actions/workflows/")))).toHaveLength(2);
  });
  it("ready refuses before GitHub when the checkout is not the preflight commit", () => {
    const {run, calls} = fixture({local: "c".repeat(40)});
    expect(() => runReleaseStage({...options, stage: "ready"}, run)).toThrow(/Checkout differs/);
    expect(calls.filter(call => call[0] === "gh")).toHaveLength(0);
  });
  it.each(["releases/tags/", "actions/workflows/", "releases"])("does not treat an incomplete API response as an absent release: %s", fail => {
    const {run, calls} = fixture({fail});
    expect(() => runReleaseStage(options, run)).toThrow(/incomplete response/);
    expect(calls.some(call => call.includes("POST"))).toBe(false);
  });
  it.each(["", `${"c".repeat(40)}\trefs/tags/${tag}\n`, `${commit}\trefs/heads/main\n`])("refuses missing, moved, or malformed remote tag %s", remote => {
    const {run, calls} = fixture({remote});
    expect(() => runReleaseStage(options, run)).toThrow(/tag/);
    expect(calls.some(call => call[0] === "gh")).toBe(false);
  });
  it("refuses a different checkout before reaching GitHub", () => {
    const {run, calls} = fixture({local: "c".repeat(40)});
    expect(() => runReleaseStage(options, run)).toThrow(/Checkout differs/);
    expect(calls).toHaveLength(1);
  });
  it("checks the prepared release ID before uploads", () => {
    expect(() => runReleaseStage({...options, stage: "check", releaseId: "43"}, fixture().run)).toThrow(/ID changed/);
  });
  const finalize = {...options, stage: "finalize", releaseId: "42", notes: "notes\n".repeat(9_000), ...bundleDeps()};
  it("round trips large notes without environment variables and leaves a draft", () => {
    expect(runReleaseStage(finalize, fixture().run).stage).toBe("finalize");
  });
  it("downloads and opens every bundled installer plus SHA256SUMS.txt before the notes are written, and reports what was not opened", () => {
    const order: string[] = [];
    const {run} = fixture();
    const spy: Runner = (program, args, input) => { if (args[3] === "PATCH") order.push("patch"); return run(program, args, input); };
    const deps = bundleDeps({order});
    const result = runReleaseStage({...finalize, ...deps}, spy);
    expect(deps.fetched.sort()).toEqual([...bundleSpecs("1.2.3").map(spec => spec.asset), checksumsAssetName].sort());
    expect(order).toEqual(["inspect", "patch"]);
    expect(deps.inspected[0].binaries).toEqual(binaries);
    expect(result.bundles).toBe("opened 7 of 12 installers; AppImage, dmg and rpm are not opened");
    expect(existsSync(deps.downloadDir) && readdirSync(deps.downloadDir)).toEqual([]);
  });
  it("refuses stale or tampered SHA256SUMS.txt metadata before downloading any installer, and refuses corrupted SHA256SUMS.txt bytes on download", () => {
    // 1. Metadata mismatch (stale SHA256SUMS.txt on draft) fails fast with 0 downloads and 0 patches.
    const stale = draft();
    stale.assets = stale.assets.map(asset =>
      asset.name === checksumsAssetName ? {...asset, digest: `sha256:${"e".repeat(64)}`} : asset
    );
    const staleFix = fixture({release: stale});
    const staleDeps = bundleDeps();
    expect(() => runReleaseStage({...finalize, ...staleDeps}, staleFix.run)).toThrow(/SHA256SUMS manifest .* does not match the current draft assets/);
    expect(staleDeps.fetched).toEqual([]);
    expect(staleFix.patches).toHaveLength(0);

    // 2. Corrupted downloaded SHA256SUMS.txt bytes fail before inspecting bundles or writing notes.
    const corruptFix = fixture();
    const corruptDeps = bundleDeps({corrupt: checksumsAssetName});
    expect(() => runReleaseStage({...finalize, ...corruptDeps}, corruptFix.run)).toThrow(/SHA256SUMS manifest .* does not match GitHub's SHA-256/);
    expect(corruptDeps.inspected).toHaveLength(0);
    expect(corruptFix.patches).toHaveLength(0);
  });
  it.each([
    ["a digest that is not GitHub's", {corrupt: "Waylode_1.2.3_amd64.deb"}, /does not match GitHub's SHA-256/],
    ["a short download", {short: "Waylode_1.2.3_x64_en-US.msi"}, /downloaded \d+ bytes, expected \d+/],
    ["a download that fails", {fail: "Waylode_1.2.3_universal.app.tar.gz"}, /download refused/],
  ])("refuses %s before opening anything or writing notes", (_label, fault, expected) => {
    const {run, patches} = fixture();
    const deps = bundleDeps(fault);
    expect(() => runReleaseStage({...finalize, ...deps}, run)).toThrow(expected);
    expect(patches).toHaveLength(0);
    expect(deps.inspected).toHaveLength(0);
  });
  it("gives each download min(120 s, the time left) and refuses by name when the budget is used up, on a fake clock", () => {
    let clock = 0;
    const {run, patches} = fixture();
    const deps = bundleDeps();
    const timeouts: (number | undefined)[] = [];
    const slow = {
      ...deps,
      fetchAsset: (request: {asset: {name: string}; dest: string; timeoutMs?: number}) => {
        timeouts.push(request.timeoutMs);
        clock += 200_000;
        deps.fetchAsset(request);
      },
    };
    const request = {...finalize, ...slow, now: () => clock, bundleBudgetMs: 700_000};
    expect(() => runReleaseStage(request, run)).toThrow("bundle check budget exhausted after 4 of 7 bundles");
    // 700 s: three full 120 s caps, then the 4th gets the 100 s left, and the 5th starts with none.
    expect(timeouts).toEqual([120_000, 120_000, 120_000, 100_000]);
    expect(patches).toHaveLength(0);
    expect(deps.inspected).toHaveLength(0);
    expect(readdirSync(deps.downloadDir)).toEqual([]);
  });
  it("names the budget when a download is killed by the limit the budget clamped it to", () => {
    let clock = 0;
    const {run} = fixture();
    const deps = bundleDeps();
    const killed = {...deps, fetchAsset: () => { clock += 130_000; throw new Error("Download of x failed: spawnSync gh ETIMEDOUT"); }};
    expect(() => runReleaseStage({...finalize, ...killed, now: () => clock, bundleBudgetMs: 130_000}, run))
      .toThrow("bundle check budget exhausted after 0 of 7 bundles");
  });
  it("hands the same budget to the inspection, and surfaces its exhaustion as a refusal", () => {
    let clock = 0;
    const {run, patches} = fixture();
    const deps = bundleDeps();
    let handed: {callTimeout: (cap: number) => number} | undefined;
    const inspect = (input: {budget?: {callTimeout: (cap: number) => number}}) => {
      handed = input.budget;
      clock += 500_000;
      return {ok: false, invalid: true, violations: ["bundle check budget exhausted after 5 of 7 bundles"], opened: [], coverage: "opened 5 of 12 installers"};
    };
    expect(() => runReleaseStage({...finalize, ...deps, inspect, now: () => clock, bundleBudgetMs: 400_000}, run))
      .toThrow(/bundle check budget exhausted after 5 of 7 bundles/);
    // The inspector was given the one deadline, and the fake clock has passed it.
    expect(handed).toBeDefined();
    expect(() => handed?.callTimeout(1)).toThrow();
    expect(patches).toHaveLength(0);
  });
  it("refuses an asset that is larger than the download bound", () => {
    const release = draft();
    release.assets = release.assets.map(asset => asset.name === "Waylode_1.2.3_amd64.deb" ? {...asset, size: 513 * 1024 * 1024} : asset);
    const {run} = fixture({release});
    const deps = bundleDeps();
    expect(() => runReleaseStage({...finalize, ...deps}, run)).toThrow(/size outside/);
    expect(deps.fetched).toEqual([]);
  });
  it("writes no notes when a binary is missing from a bundle, and says which", () => {
    const {run, patches} = fixture();
    const deps = bundleDeps({verdict: {ok: false, invalid: false, violations: ["Waylode_1.2.3_amd64.deb: missing: waylode-hook"], opened: [], coverage: "opened 7 of 12 installers"}});
    expect(() => runReleaseStage({...finalize, ...deps}, run)).toThrow(/Waylode_1\.2\.3_amd64\.deb: missing: waylode-hook/);
    expect(patches).toHaveLength(0);
  });
  it("refuses a bundle whose bytes change while they are being opened", () => {
    const {run, patches} = fixture();
    const deps = bundleDeps({swapDuringInspect: true});
    expect(() => runReleaseStage({...finalize, ...deps}, run)).toThrow(/changed while it was being inspected/);
    expect(patches).toHaveLength(0);
  });
  it("refuses to finalize without a download directory or the declared binaries, before any GitHub call", () => {
    for (const missing of [{downloadDir: undefined}, {downloadDir: ""}, {binaries: undefined}, {binaries: []}]) {
      const {run, calls} = fixture();
      expect(() => runReleaseStage({...finalize, ...missing}, run)).toThrow(/finalize needs/);
      expect(calls).toHaveLength(0);
    }
  });
  it("writes the intended tag name back when GitHub left the draft untagged", () => {
    const {run, patches} = fixture({release: {...draft(), tag_name: "untagged-f782e62dab083869a408"}});
    expect(runReleaseStage(finalize, run).stage).toBe("finalize");
    expect(patches.some(body => JSON.parse(body).tag_name === tag && JSON.parse(body).body === finalize.notes)).toBe(true);
  });
  it("ignores download counters and API ordering when verifying stable asset identity", () => {
    expect(runReleaseStage(finalize, fixture({downloadDuringFinalize: true}).run).stage).toBe("finalize");
  });
  it.each([
    {corruptNotes: true}, {changeAssets: true}, {publishAfterPatch: true},
    {release: {...draft(), assets: []}},
    {release: {...draft(), assets: draft().assets.map(asset => ({...asset, id: 1}))}},
    {release: {...draft(), assets: draft().assets.map(asset => ({...asset, digest: null}))}},
  ])("refuses incomplete or changing finalization evidence %j", change => {
    expect(() => runReleaseStage(finalize, fixture(change).run)).toThrow();
  });
  it.each([{tag: ""}, {commit: "main"}, {repo: "../escape"}, {stage: "publish"}])("refuses invalid inputs without subprocesses %j", change => {
    const {run, calls} = fixture();
    expect(() => runReleaseStage({...options, ...change}, run)).toThrow();
    expect(calls).toHaveLength(0);
  });
  it("treats 404 with any non-zero exit code as absent when allowMissing is true", () => {
    const custom404Run: Runner = (program, args, input) => {
      if (program === "gh" && String(args[4] ?? "").includes("releases/tags/")) {
        return {status: 4, failed: false, stdout: "HTTP/2.0 404 Not Found\r\nContent-Type: application/json\r\n\r\n{\"message\":\"Not Found\"}"};
      }
      return fixture({release: null}).run(program, args, input);
    };
    expect(runReleaseStage(options, custom404Run).release_id).toBe("42");
  });
  it("treats 404 with exit code 0 as absent when allowMissing is true", () => {
    const custom404ZeroRun: Runner = (program, args, input) => {
      if (program === "gh" && String(args[4] ?? "").includes("releases/tags/")) {
        return {status: 0, failed: false, stdout: "HTTP/2.0 404 Not Found\r\nContent-Type: application/json\r\n\r\n{\"message\":\"Not Found\"}"};
      }
      return fixture({release: null}).run(program, args, input);
    };
    expect(runReleaseStage(options, custom404ZeroRun).release_id).toBe("42");
  });
  it("accepts CRLF line endings returned by GitHub in confirmed notes", () => {
    expect(runReleaseStage(finalize, fixture({crlfNotes: true}).run).stage).toBe("finalize");
  });
  it("accepts uppercase hex in asset SHA-256 digest", () => {
    const uppercaseDraft = {
      ...draft(),
      assets: draft().assets.map(asset => ({
        ...asset,
        digest: `sha256:${stubDigest(asset.name).slice("sha256:".length).toUpperCase()}`,
      })),
    };
    const { run } = fixture({ release: uppercaseDraft, list: [uppercaseDraft] });
    expect(runReleaseStage(finalize, run).stage).toBe("finalize");
  });
  // Driven through an injected runner rather than the real `git`. The point is
  // which identifier each source wins from, and spawning to learn that measured
  // the host instead: it reached the network through `gh` and timed out on a
  // Windows runner, where process creation is dearer, while passing elsewhere.
  const gitRun = (out: Record<string, string>): Runner => (program, args) => {
    const key = `${program} ${args.join(" ")}`;
    return {status: key in out ? 0 : 1, failed: false, stdout: out[key] ?? ""};
  };
  const ORIGIN = "git config --get remote.origin.url";
  const HEAD = "git rev-parse HEAD";
  const POINTS_AT = "git tag --points-at HEAD";

  it("resolves repository, commit, and tag from local git when env vars are unset", () => {
    expect(resolveReleaseContext({}, gitRun({
      [ORIGIN]: "https://github.com/owner/repo.git\n",
      [HEAD]: `${commit}\n`,
      [POINTS_AT]: `${tag}\n`,
    }))).toEqual({repo: "owner/repo", commit, tag});
  });

  it("resolves an SSH remote and a checkout carrying several tags", () => {
    expect(resolveReleaseContext({}, gitRun({
      [ORIGIN]: "git@github.com:owner/repo.git\n",
      [HEAD]: `${commit}\n`,
      // Only the vMAJOR.MINOR.PATCH one names a release; the draft leftover
      // `untagged-<hex>` shape sits beside it in real checkouts.
      [POINTS_AT]: `untagged-2db6b2cc5c\n${tag}\n`,
    }))).toEqual({repo: "owner/repo", commit, tag});
  });

  it("prefers the environment over git, so CI stays authoritative", () => {
    const env = {GH_REPO: "ci/owner", RELEASE_COMMIT: "b".repeat(40), RELEASE_TAG: "v9.9.9"};
    expect(resolveReleaseContext(env, gitRun({
      [ORIGIN]: "https://github.com/owner/repo.git\n",
      [HEAD]: `${commit}\n`,
      [POINTS_AT]: `${tag}\n`,
    }))).toEqual({repo: "ci/owner", commit: "b".repeat(40), tag: "v9.9.9"});
  });

  it("falls back to the package version when HEAD carries no release tag", () => {
    const resolved = resolveReleaseContext({}, gitRun({
      [ORIGIN]: "https://github.com/owner/repo.git\n",
      [HEAD]: `${commit}\n`,
      [POINTS_AT]: "\n",
    }));
    // Read from this repository's own package.json, so it tracks the release
    // rather than pinning a number this test would have to be told about.
    const version = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")).version;
    expect(resolved.tag).toBe(`v${version}`);
  });

  it("resolves a beta release tag pointing at HEAD instead of falling back to package.json", () => {
    expect(resolveReleaseContext({}, gitRun({
      [ORIGIN]: "https://github.com/owner/repo.git\n",
      [HEAD]: `${commit}\n`,
      [POINTS_AT]: "untagged-2db6b2cc5c\nv0.2.0-beta.1\n",
    }))).toEqual({repo: "owner/repo", commit, tag: "v0.2.0-beta.1"});
    // When both a beta and stable tag sit on HEAD, the stable tag wins.
    expect(resolveReleaseContext({}, gitRun({
      [ORIGIN]: "https://github.com/owner/repo.git\n",
      [HEAD]: `${commit}\n`,
      [POINTS_AT]: "v0.2.0-beta.1\nv0.2.0\n",
    })).tag).toBe("v0.2.0");
  });

  it("leaves an unresolvable remote empty rather than guessing a repository", () => {
    expect(resolveReleaseContext({}, gitRun({
      [ORIGIN]: "https://example.invalid/not-github.git\n",
      [HEAD]: `${commit}\n`,
      [POINTS_AT]: `${tag}\n`,
    })).repo).toBe("");
  });

  it("main still refuses a stage it cannot resolve a repository for", () => {
    // The exit-code contract the removed end-to-end case was really asserting,
    // without a network round trip: a refusal prints and returns 1.
    expect(main(["prepare", "extra"])).toBe(1);
  });
});

describe("attest stage", () => {
  const sboms = {spdx: '{"spdxVersion":"SPDX-2.3"}', cargo: '{"bomFormat":"CycloneDX"}'};
  const attest = {stage: "attest", repo: "owner/repo", tag, commit, releaseId: "42", sboms};
  const names = supplementNames("1.2.3");
  const sha = (text: string) => createHash("sha256").update(text, "utf8").digest("hex");
  function installersOnly(extra: {id: number, name: string}[] = []) {
    const release = draft();
    const installers = new Set(expectedInstallerNames("1.2.3"));
    return {...release, assets: [...release.assets.filter(asset => installers.has(asset.name)),
      ...extra.map(asset => ({...asset, size: 1, state: "uploaded", digest: `sha256:${"d".repeat(64)}`}))]};
  }
  const uploads = (calls: string[][]) => calls.filter(call => String(call[5] ?? "").startsWith("https://uploads.github.com/"));

  it("uploads both SBOMs and a SHA-256 manifest covering every installer and SBOM", () => {
    const {run, calls} = fixture({release: installersOnly()});
    expect(runReleaseStage(attest, run).release_id).toBe("42");
    const posted = uploads(calls).map(call => decodeURIComponent(call[5].split("?name=")[1]));
    expect(posted).toEqual([names.spdx, names.cargo, names.checksums]);

    const sumsCall = calls.find(call => String(call[5] ?? "").endsWith(encodeURIComponent(names.checksums)));
    expect(sumsCall).toContain("Content-Type: text/plain");
  });

  it("writes a sha256sum -c manifest whose lines are the GitHub installer digests and local SBOM hashes", () => {
    let manifest = "";
    const {run} = fixture({release: installersOnly()});
    const spy: Runner = (program, args, input) => {
      if (String(args[4] ?? "").endsWith(encodeURIComponent(names.checksums))) manifest = input ?? "";
      return run(program, args, input);
    };
    runReleaseStage(attest, spy);
    const lines = manifest.trimEnd().split("\n");
    expect(lines).toHaveLength(expectedInstallerNames("1.2.3").length + 2);
    expect(lines).toContain(`${stubDigest("Waylode_1.2.3_arm64_en-US.msi").slice("sha256:".length)}  Waylode_1.2.3_arm64_en-US.msi`);
    expect(lines).toContain(`${sha(sboms.spdx)}  ${names.spdx}`);
    expect(lines).toContain(`${sha(sboms.cargo)}  ${names.cargo}`);
    expect(lines.every(line => /^[a-f0-9]{64} {2}Waylode[-_]1\.2\.3[-_]/.test(line))).toBe(true);
    expect([...lines].sort((a, b) => compareCodePoints(a.slice(66), b.slice(66)))).toEqual(lines);
  });

  it("validates formatChecksumsManifest, parseChecksumsManifest, and verifyChecksumsManifest strictly", () => {
    const hexA = "a".repeat(64);
    const hexB = "b".repeat(64);
    // Code-point sort puts '-' (0x2d) < '.' (0x2e) < '_' (0x5f) deterministically across all locales.
    const formatted = formatChecksumsManifest([
      {name: "Waylode_1.2.3_arm64_en-US.msi", digest: `sha256:${hexB.toUpperCase()}`},
      {name: "Waylode_1.2.3_arm64-setup.exe", digest: hexA},
    ]);
    expect(formatted).toBe(`${hexA}  Waylode_1.2.3_arm64-setup.exe\n${hexB}  Waylode_1.2.3_arm64_en-US.msi\n`);
    expect(() => formatChecksumsManifest([{name: "a", digest: hexA}, {name: "a", digest: hexB}])).toThrow(/Duplicate/);
    expect(() => formatChecksumsManifest([{name: "bad name", digest: hexA}])).toThrow(/Invalid checksums manifest filename/);
    expect(() => parseChecksumsManifest(`${hexA}  a\r\n`)).toThrow(/CR line endings/);
    expect(() => parseChecksumsManifest(`${hexA}  a`)).toThrow(/end with a newline/);
    expect(() => parseChecksumsManifest(`${hexA.toUpperCase()}  a\n`)).toThrow(/malformed/);
    expect(() => parseChecksumsManifest(`${hexA}  a\n${hexB}  a\n`)).toThrow(/duplicate entry/);

    const fullDraft = draft();
    expect(verifyChecksumsManifest({version: "1.2.3", assets: fullDraft.assets, manifestText: canonicalSumsBytes.toString("utf8")}).size).toBe(14);
    // Reversed line order fails canonical format check.
    const reversed = `${canonicalSumsBytes.toString("utf8").trimEnd().split("\n").reverse().join("\n")}\n`;
    expect(() => verifyChecksumsManifest({version: "1.2.3", assets: fullDraft.assets, manifestText: reversed})).toThrow(/canonical sorted format/);
    // Self-reference to SHA256SUMS.txt fails exact coverage check.
    const withSelf = `${canonicalSumsBytes.toString("utf8")}${hexA}  ${checksumsAssetName}\n`;
    expect(() => verifyChecksumsManifest({version: "1.2.3", assets: fullDraft.assets, manifestText: withSelf})).toThrow(/coverage is not exact/);
  });

  it("refuses duplicate installer names, zero-byte installers, or non-uploaded installer states before uploading any supplement", () => {
    const dup = installersOnly();
    dup.assets = [...dup.assets, {...dup.assets[0], id: 999}];
    const dupFix = fixture({release: dup});
    expect(() => runReleaseStage(attest, dupFix.run)).toThrow(/duplicate:/);
    expect(uploads(dupFix.calls)).toHaveLength(0);

    const zero = installersOnly();
    zero.assets = zero.assets.map((a, idx) => (idx === 0 ? {...a, size: 0} : a));
    const zeroFix = fixture({release: zero});
    expect(() => runReleaseStage(attest, zeroFix.run)).toThrow(/invalid size/);
    expect(uploads(zeroFix.calls)).toHaveLength(0);

    const starter = installersOnly();
    starter.assets = starter.assets.map((a, idx) => (idx === 0 ? {...a, state: "starter"} : a));
    const starterFix = fixture({release: starter});
    expect(() => runReleaseStage(attest, starterFix.run)).toThrow(/not uploaded/);
    expect(uploads(starterFix.calls)).toHaveLength(0);

    const overflow = installersOnly();
    overflow.assets = Array.from({length: 100}, (_, idx) => ({
      id: 2000 + idx, name: `extra-${idx}.bin`, size: 10, state: "uploaded", digest: `sha256:${"a".repeat(64)}`,
    }));
    const overflowFix = fixture({release: overflow});
    expect(() => runReleaseStage(attest, overflowFix.run)).toThrow(/reached 100 entries and may be truncated/);
    expect(uploads(overflowFix.calls)).toHaveLength(0);
  });

  it("replaces its own earlier uploads on a rerun and never deletes an installer", () => {
    const {run, calls} = fixture({release: installersOnly([{id: 900, name: names.checksums}, {id: 901, name: names.spdx}])});
    runReleaseStage(attest, run);
    const deleted = calls.filter(call => call.includes("DELETE")).map(call => call[5]);
    expect(deleted.sort()).toEqual(["repos/owner/repo/releases/assets/900", "repos/owner/repo/releases/assets/901"]);
    expect(uploads(calls)).toHaveLength(3);
  });

  it("recovers on rerun when an interrupted upload left a starter supplement with null digest", () => {
    const release = installersOnly();
    release.assets = [
      ...release.assets,
      {id: 902, name: names.spdx, size: 0, state: "starter", digest: null as unknown as string},
    ];
    const {run, calls} = fixture({release});
    expect(runReleaseStage(attest, run).release_id).toBe("42");
    const deleted = calls.filter(call => call.includes("DELETE")).map(call => call[5]);
    expect(deleted).toEqual(["repos/owner/repo/releases/assets/902"]);
    expect(uploads(calls)).toHaveLength(3);
  });

  it("retries transient 5xx supplement uploads after wiping partial starter leftovers and fails fast on 4xx", () => {
    expect(isTransientUploadError("GitHub POST upload x: HTTP 502")).toBe(true);
    expect(isTransientUploadError("GitHub POST upload x: incomplete response")).toBe(true);
    expect(isTransientUploadError("GitHub POST upload x: HTTP 422")).toBe(false);
    expect(isTransientUploadError("GitHub POST upload x: HTTP 403")).toBe(false);

    const flaky = fixture({release: installersOnly(), flakyUploads: 2});
    expect(runReleaseStage(attest, flaky.run).release_id).toBe("42");
    expect(uploads(flaky.calls)).toHaveLength(5);
    const deletedIds = flaky.calls.filter(call => call.includes("DELETE")).map(call => call[5]);
    expect(deletedIds).toHaveLength(2);
    // Never deleted any installer ID (1..12).
    for (const del of deletedIds) expect(Number(del.split("/").pop())).toBeGreaterThanOrEqual(1900);

    const perm = fixture({release: installersOnly(), uploadStatus: 422});
    expect(() => runReleaseStage(attest, perm.run)).toThrow(/HTTP 422/);
    expect(uploads(perm.calls)).toHaveLength(1);
  });

  it("refuses before uploading when a platform's installers are missing", () => {
    const release = installersOnly();
    release.assets = release.assets.filter(asset => !asset.name.includes("arm64"));
    const {run, calls} = fixture({release});
    expect(() => runReleaseStage(attest, run)).toThrow(/missing: Waylode_1\.2\.3_arm64-setup\.exe/);
    expect(uploads(calls)).toHaveLength(0);
  });

  it("refuses an unknown asset rather than leaving it outside the manifest", () => {
    const {run, calls} = fixture({release: installersOnly([{id: 950, name: "stray.zip"}])});
    expect(() => runReleaseStage(attest, run)).toThrow(/unexpected: stray\.zip/);
    expect(uploads(calls)).toHaveLength(0);
  });

  it("fails when GitHub's digest of an upload differs from the local SHA-256", () => {
    const {run} = fixture({release: installersOnly(), corruptUpload: true});
    expect(() => runReleaseStage(attest, run)).toThrow(/does not match its local SHA-256/);
  });

  it("refuses an upload URL that names another release", () => {
    const release = {...installersOnly(), upload_url: "https://uploads.github.com/repos/owner/repo/releases/7/assets{?name,label}"};
    const {run, calls} = fixture({release});
    expect(() => runReleaseStage(attest, run)).toThrow(/upload URL/);
    expect(uploads(calls)).toHaveLength(0);
  });

  it.each([{spdx: "", cargo: "{}"}, {spdx: "{}", cargo: ""}, undefined])("refuses a missing SBOM before reaching GitHub", missing => {
    const {run, calls} = fixture({release: installersOnly()});
    expect(() => runReleaseStage({...attest, sboms: missing}, run)).toThrow(/SBOM/);
    expect(calls).toHaveLength(0);
  });

  it.each([
    ["non-JSON SPDX", {spdx: "not-json", cargo: sboms.cargo}, /Invalid SPDX SBOM/],
    ["SPDX missing spdxVersion", {spdx: '{"name":"gitpulse"}', cargo: sboms.cargo}, /spdxVersion/],
    ["SPDX non-array packages", {spdx: '{"spdxVersion":"SPDX-2.3","packages":"bad"}', cargo: sboms.cargo}, /packages must be an array/],
    ["non-JSON CycloneDX", {spdx: sboms.spdx, cargo: "<html>502</html>"}, /Invalid CycloneDX SBOM/],
    ["CycloneDX with wrong bomFormat", {spdx: sboms.spdx, cargo: '{"bomFormat":"SPDX"}'}, /CycloneDX/],
    ["CycloneDX non-array components", {spdx: sboms.spdx, cargo: '{"bomFormat":"CycloneDX","components":{}}'}, /components must be an array/],
  ])("refuses structurally invalid SBOM (%s) before reaching GitHub", (_label, badSboms, expected) => {
    const {run, calls} = fixture({release: installersOnly()});
    expect(() => validateSbomDocuments(badSboms)).toThrow(expected);
    expect(() => runReleaseStage({...attest, sboms: badSboms}, run)).toThrow(expected);
    expect(calls).toHaveLength(0);
  });

  it("leaves finalize an exact inventory of installers plus supplements", () => {
    const {run, uploadedBytes} = fixture({release: installersOnly()});
    const finalizing = {...attest, stage: "finalize", notes: "notes", ...bundleDeps({bytesOverride: uploadedBytes})};
    expect(() => runReleaseStage(finalizing, run)).toThrow(/missing: Waylode_1\.2\.3_SHA256SUMS\.txt/);
    runReleaseStage(attest, run);
    expect(runReleaseStage(finalizing, run).release_id).toBe("42");
  });
});

/**
 * Adversarial cases for ft-ac3bba92 (adversary pass, build run 2026-10-26):
 * the real `fetchReleaseAsset` against a stand-in `gh` on PATH. Both held.
 */
describe.skipIf(process.platform === "win32")("finalize with the real download and a misbehaving gh", () => {
  const withGh = (mode: string, body: () => void) => {
    const bin = mkdtempSync(path.join(tmpdir(), "gitpulse-fake-gh-"));
    scratch.push(bin);
    const gh = path.join(bin, "gh");
    writeFileSync(gh, [
      "#!/bin/bash",
      'case "$FAKE_GH_MODE" in',
      '  notfound) echo \'{"message":"Not Found"}\'; echo "gh: Not Found (HTTP 404)" >&2; exit 1;;',
      "  partial) printf abc; exit 0;;",
      "  redirect-page) echo '<html><body>Found. Redirecting to https://objects.example/x</body></html>'; exit 0;;",
      "esac",
      "",
    ].join("\n"), {mode: 0o755});
    const previous = {path: process.env.PATH, mode: process.env.FAKE_GH_MODE};
    process.env.PATH = `${bin}${path.delimiter}${process.env.PATH ?? ""}`;
    process.env.FAKE_GH_MODE = mode;
    try {
      body();
    } finally {
      process.env.PATH = previous.path;
      if (previous.mode === undefined) delete process.env.FAKE_GH_MODE;
      else process.env.FAKE_GH_MODE = previous.mode;
    }
  };

  it.each([
    ["a 404 from the download", "notfound", /Download of Waylode_1\.2\.3_\S+ failed: .*404/],
    ["a body that stops after three bytes", "partial", /downloaded 3 bytes, expected \d+/],
    ["an HTML page where the bytes should be", "redirect-page", /downloaded \d+ bytes, expected \d+|does not match GitHub's SHA-256/],
  ])("HOLDS: refuses %s before any inspection or notes, and leaves no file behind", (_label, mode, expected) => {
    const {run, patches} = fixture();
    const deps = bundleDeps();
    const inspected: unknown[] = [];
    const request = {...options, stage: "finalize", releaseId: "42", notes: "notes", downloadDir: deps.downloadDir, binaries,
      inspect: (input: unknown) => { inspected.push(input); throw new Error("must not be reached"); }};
    withGh(mode, () => expect(() => runReleaseStage(request, run)).toThrow(expected));
    expect(patches).toHaveLength(0);
    expect(inspected).toHaveLength(0);
    expect(readdirSync(deps.downloadDir)).toEqual([]);
  });

  it("retries transient 5xx download failures inside the timeout and succeeds once GitHub recovers", () => {
    expect(isPermanentDownloadFailure("gh: Not Found (HTTP 404)")).toBe(true);
    expect(isPermanentDownloadFailure("gh: Unauthorized (HTTP 401)")).toBe(true);
    expect(isPermanentDownloadFailure("spawnSync gh ENOENT", "ENOENT")).toBe(true);
    expect(isPermanentDownloadFailure("gh: Bad Gateway (HTTP 502)")).toBe(false);
    expect(isPermanentDownloadFailure("connection reset by peer")).toBe(false);

    const work = mkdtempSync(path.join(tmpdir(), "gitpulse-retry-gh-"));
    scratch.push(work);
    const bin = path.join(work, "bin");
    const counterFile = path.join(work, "attempts.txt");
    const dest = path.join(work, "asset.bin");
    writeFileSync(counterFile, "0");
    const gh = path.join(bin, "gh");
    // Create bin dir and flaky gh: fails with partial stdout + HTTP 502 on attempt 1, succeeds on attempt 2.
    mkdirSync(bin, { recursive: true });
    writeFileSync(gh, [
      "#!/bin/bash",
      `count=$(cat "${counterFile}")`,
      "count=$((count + 1))",
      `echo "$count" > "${counterFile}"`,
      'if [ "$count" -eq 1 ]; then',
      '  printf "partial-junk"',
      '  echo "gh: Bad Gateway (HTTP 502)" >&2',
      "  exit 1",
      "fi",
      'printf "good-payload"',
      "",
    ].join("\n"), { mode: 0o755 });

    const prevPath = process.env.PATH;
    process.env.PATH = `${bin}${path.delimiter}${process.env.PATH ?? ""}`;
    try {
      fetchReleaseAsset({
        repo: "owner/repo",
        asset: { id: 99, name: "Waylode_1.2.3_amd64.deb", size: 12, digest: `sha256:${"a".repeat(64)}` },
        dest,
        timeoutMs: 10_000,
      });
      expect(readFileSync(dest, "utf8")).toBe("good-payload");
      expect(readFileSync(counterFile, "utf8").trim()).toBe("2");
    } finally {
      process.env.PATH = prevPath;
    }
  });
});
