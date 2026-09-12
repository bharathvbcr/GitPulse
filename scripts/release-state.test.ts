import { describe, expect, it } from "vitest";
import { expectedAssetNames } from "./check-release-assets.mjs";
import { runReleaseStage, runCommand, type Runner } from "./release-state.mjs";

const commit = "a".repeat(40);
const tag = "v1.2.3";
const options = { stage: "prepare", repo: "owner/repo", tag, commit };
function draft() {
  return {id: 42, tag_name: tag, name: `GitPulse ${tag}`, target_commitish: commit, draft: true, prerelease: false,
    immutable: false, published_at: null, body: "pending", assets: expectedAssetNames("1.2.3")
      .map((name, index) => ({id: index + 1, name, size: 123, state: "uploaded", digest: `sha256:${"b".repeat(64)}`}))};
}
function fixture(change: {
  release?: Record<string, unknown> | null;
  ci?: Record<string, unknown>;
  remote?: string;
  local?: string;
  fail?: string;
  corruptNotes?: boolean;
  changeAssets?: boolean;
  publishAfterPatch?: boolean;
  apiStatus?: number;
  uncertainPost?: boolean;
  downloadDuringFinalize?: boolean;
  list?: unknown[];
} = {}) {
  let release = change.release === undefined ? draft() : change.release;
  const calls: string[][] = [];
  const patches: string[] = [];
  let patched = false;
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
    if (endpoint.includes("actions/workflows/")) return respond({workflow_runs: [change.ci ?? {head_sha: commit, event: "push", status: "completed", conclusion: "success"}]});
    if (args[3] === "DELETE") {
      const assetMatch = /releases\/assets\/(\d+)$/.exec(String(args[4] ?? ""));
      if (assetMatch && release && Array.isArray(release.assets)) {
        const id = Number(assetMatch[1]);
        release = {...release, assets: release.assets.filter(asset => asset.id !== id)};
      }
      return {status: 0, failed: false, stdout: "HTTP/2.0 204 No Content\r\n\r\n"};
    }
    if (args[3] === "POST") { release = {...draft(), ...JSON.parse(input ?? "{}")}; return change.uncertainPost ? {status: null, failed: true, stdout: ""} : respond(release, 201); }
    if (args[3] === "PATCH") {
      patched = true;
      patches.push(input ?? "");
      release = {...release, ...JSON.parse(input ?? "{}")};
      if (change.corruptNotes && release) release.body = "damaged";
      if (change.publishAfterPatch && release) release.draft = false;
      return respond(release);
    }
    if (String(args[4] ?? "").includes("releases?per_page=")) {
      return respond(change.list ?? (release ? [release] : []));
    }
    if (patched && change.changeAssets && release) release.assets = [];
    if (patched && change.downloadDuringFinalize && release) release.assets = draft().assets.map(asset => ({...asset, download_count: 1})).reverse();
    return release ? respond(release) : respond({message: "Not Found"}, 404);
  };
  return {run, calls, patches};
}

describe("remote release lifecycle", () => {
  it("creates a draft only after successful CI and coverage for the pinned commit", () => {
    const {run, calls} = fixture({release: null});
    expect(runReleaseStage(options, run).release_id).toBe("42");
    expect(calls.filter(call => call.includes("POST"))).toHaveLength(1);
    expect(calls.filter(call => call.some(arg => arg.includes("actions/workflows/")))).toHaveLength(2);
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
  it("refuses a truncated release list rather than posting a duplicate", () => {
    const list = Array.from({length: 100}, (_, index) => ({...draft(), id: index + 1, name: `Other ${index}`}));
    const {run, calls} = fixture({apiStatus: 404, release: null, list});
    expect(() => runReleaseStage(options, run)).toThrow(/capped at 100/);
    expect(calls.some(call => call.includes("POST"))).toBe(false);
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
        {id: 11, name: "GitPulse_1.2.3_x64-setup.exe", size: 10, state: "uploaded", digest: `sha256:${"b".repeat(64)}`},
        {id: 12, name: "GitPulse_1.2.3_x64_en-US.msi", size: 10, state: "uploaded", digest: `sha256:${"b".repeat(64)}`},
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
      assets: [{name: "GitPulse_1.2.3_x64-setup.exe", size: 10, state: "uploaded", digest: `sha256:${"b".repeat(64)}`}],
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
  const finalize = {...options, stage: "finalize", releaseId: "42", notes: "notes\n".repeat(9_000)};
  it("round trips large notes without environment variables and leaves a draft", () => {
    expect(runReleaseStage(finalize, fixture().run).stage).toBe("finalize");
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
});
