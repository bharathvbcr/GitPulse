import { describe, expect, it } from "vitest";
import {
  formatReleaseDate,
  releaseTagSuggestion,
  summarizeCommitReview,
  splitBranchCleanup,
  summarizeReleases,
  type BranchCleanupCandidate,
  type CommitReviewReport,
  type ReleaseInfo,
} from "./model";

describe("MANVI ops helpers", () => {
  it("suggests the next stable patch without being confused by prereleases", () => {
    expect(releaseTagSuggestion(["v1.2.9", "v1.3.0-beta.1", "not-a-release"])).toBe("v1.2.10");
    expect(releaseTagSuggestion([])).toBe("v0.0.1");
  });

  it("formats release dates gracefully", () => {
    expect(formatReleaseDate("")).toBe("");
    expect(formatReleaseDate("2026-08-25T12:00:00Z")).toContain("2026");
    expect(formatReleaseDate("not-a-date")).toBe("not-a-date");
  });

  it("summarizes releases with and without capping", () => {
    expect(summarizeReleases([])).toBe("No releases");
    const releases: ReleaseInfo[] = [
      {
        tag_name: "v1.0.0",
        name: "v1.0.0",
        is_draft: false,
        is_prerelease: false,
        is_latest: true,
        published_at: "2026-08-25T12:00:00Z",
        created_at: "2026-08-25T12:00:00Z",
        url: "https://github.com/acme/repo/releases/tag/v1.0.0",
      },
    ];
    expect(summarizeReleases(releases, false)).toBe("1 release");
    expect(summarizeReleases(releases, true)).toBe("1 release (capped)");
  });

  it("keeps capped commit review coverage explicit", () => {
    const report: CommitReviewReport = {
      range: "origin/main..HEAD",
      total_commits: 620,
      reviewed_commits: 500,
      truncated: true,
      conventional_commits: 410,
      issue_linked_commits: 120,
      findings: [],
    };

    expect(summarizeCommitReview(report)).toContain("500 of 620");
    expect(summarizeCommitReview(report)).toContain("capped");
  });

  it("reports complete review coverage without a capped warning", () => {
    const report: CommitReviewReport = {
      range: "main..HEAD",
      total_commits: 3,
      reviewed_commits: 3,
      truncated: false,
      conventional_commits: 3,
      issue_linked_commits: 2,
      findings: [],
    };

    expect(summarizeCommitReview(report)).toBe("Reviewed all 3 outgoing commits.");
  });
});

describe("branch cleanup delete route", () => {
  const candidate = (name: string, merged_by_tree: boolean): BranchCleanupCandidate => ({
    name,
    tip_commit_id: `${name}-tip`,
    last_summary: "",
    last_author: "",
    last_commit_timestamp: 0,
    upstream_gone: false,
    merged_by_tree,
  });

  it("sends a tree-merged branch to the gated clean and an ancestry-merged one to -d", () => {
    const all = [candidate("anc", false), candidate("sq", true), candidate("anc2", false)];
    const route = splitBranchCleanup(all, ["sq", "anc"]);
    expect(route.ancestry.map((c) => c.name)).toEqual(["anc"]);
    expect(route.byTree.map((c) => c.name)).toEqual(["sq"]);
  });

  it("never routes a branch that was not selected", () => {
    const route = splitBranchCleanup([candidate("sq", true), candidate("anc", false)], []);
    expect(route.ancestry).toEqual([]);
    expect(route.byTree).toEqual([]);
  });
});
