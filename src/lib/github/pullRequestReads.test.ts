import { get } from "svelte/store";
import { describe, expect, it } from "vitest";
import type { GitHubContext, PullRequestInfo } from "./types";
import { MAX_REMEMBERED_REPOSITORIES, notePullRequests, pullRequestReads } from "./pullRequestReads";

const prs: PullRequestInfo[] = [{
  number: 7, title: "t", state: "OPEN", head_ref: "feat", base_ref: "main", url: "u", is_draft: false,
  ci_status: "success", created_at: "", updated_at: "", review_decision: "", first_review_at: "",
}];

function context(over: Partial<GitHubContext> = {}): GitHubContext {
  return {
    available: true, owner: "o", repo: "r", workflow_runs: [], releases: [], cli_present: true, host: "github.com",
    html_url: "", pull_requests: prs, issues: [], issues_truncated: false, ...over,
  };
}

// The store is module state shared by every test here, so each test uses its
// own repository paths.
describe("pullRequestReads", () => {
  it("remembers the pull requests of a good read, with its time", () => {
    notePullRequests("/r/good", context(), 1000);
    expect(get(pullRequestReads).get("/r/good")).toEqual({ prs, at: 1000 });
  });

  it("does not record a read that failed, was unavailable or whose pull-request listing failed, and keeps the last good one", () => {
    notePullRequests("/r/keep", context(), 1000);
    notePullRequests("/r/keep", context({ error: "rate limited", pull_requests: [] }), 2000);
    notePullRequests("/r/keep", context({ prs_error: "parse", pull_requests: [] }), 3000);
    notePullRequests("/r/keep", context({ available: false, pull_requests: [] }), 4000);
    expect(get(pullRequestReads).get("/r/keep")?.at).toBe(1000);
    notePullRequests("/r/never", context({ error: "no gh" }), 1000);
    expect(get(pullRequestReads).has("/r/never")).toBe(false);
    notePullRequests("", context(), 1000);
    expect(get(pullRequestReads).has("")).toBe(false);
  });

  it("replaces an older read with a newer one", () => {
    notePullRequests("/r/new", context(), 1000);
    notePullRequests("/r/new", context({ pull_requests: [] }), 2000);
    expect(get(pullRequestReads).get("/r/new")).toEqual({ prs: [], at: 2000 });
  });

  it("is bounded: past the cap the least recently read repository is dropped", () => {
    notePullRequests("/r/bound-first", context(), 1);
    for (let i = 0; i < MAX_REMEMBERED_REPOSITORIES; i++) notePullRequests(`/r/bound-${i}`, context(), 2 + i);
    const reads = get(pullRequestReads);
    expect(reads.size).toBe(MAX_REMEMBERED_REPOSITORIES);
    expect(reads.has("/r/bound-first")).toBe(false);
    expect(reads.has(`/r/bound-${MAX_REMEMBERED_REPOSITORIES - 1}`)).toBe(true);
  });
});
