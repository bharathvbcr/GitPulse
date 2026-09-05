import { describe, expect, it } from "vitest";
import type { IssueInfo } from "../ops/model";
import type { PullRequestInfo, WorkflowRunInfo } from "./types";
import {
  filterIssues,
  filterPullRequests,
  issueSearchText,
  PR_FACETS,
  prFacetCounts,
  prInFacet,
  prIsFailing,
  prSearchText,
  relativeAge,
  runsOnBranch,
} from "./remoteFilter";

const NOW = Date.parse("2026-09-04T12:00:00Z");

function pr(number: number, extra: Partial<PullRequestInfo> = {}): PullRequestInfo {
  return {
    number,
    title: `pull ${number}`,
    state: "OPEN",
    head_ref: `feat-${number}`,
    base_ref: "main",
    url: `https://example.test/pull/${number}`,
    is_draft: false,
    ci_status: "success",
    created_at: "2026-09-01T12:00:00Z",
    updated_at: "2026-09-02T12:00:00Z",
    review_decision: "",
    first_review_at: "2026-09-02T12:00:00Z",
    ...extra,
  };
}

function issue(number: number, extra: Partial<IssueInfo> = {}): IssueInfo {
  return {
    number,
    title: `issue ${number}`,
    state: "OPEN",
    url: `https://example.test/issues/${number}`,
    labels: [],
    updated_at: "2026-09-02T12:00:00Z",
    author: "ada",
    ...extra,
  };
}

function run(id: number, extra: Partial<WorkflowRunInfo> = {}): WorkflowRunInfo {
  return {
    id,
    name: "ci",
    title: `run ${id}`,
    status: "completed",
    conclusion: "success",
    head_branch: "main",
    url: `https://example.test/runs/${id}`,
    created_at: "2026-09-04T11:00:00Z",
    ...extra,
  };
}

describe("prIsFailing", () => {
  it("counts only a red verdict, never a run still going", () => {
    expect(prIsFailing({ ci_status: "failure" })).toBe(true);
    expect(prIsFailing({ ci_status: "TIMED_OUT" })).toBe(true);
    for (const status of ["pending", "in_progress", "queued", "waiting", "success"]) {
      expect(prIsFailing({ ci_status: status }), status).toBe(false);
    }
  });

  it("does not read an absent status as either verdict", () => {
    // A repository whose checks never start reports "", and folding that into
    // passing hides it while folding it into failing invents a red build.
    expect(prIsFailing({ ci_status: "" })).toBe(false);
    expect(prIsFailing({ ci_status: "   " })).toBe(false);
  });
});

describe("prInFacet", () => {
  const drafted = pr(1, { is_draft: true, first_review_at: "" });
  const unreviewed = pr(2, { first_review_at: "" });
  const red = pr(3, { ci_status: "failure" });
  const clean = pr(4);
  const all = [drafted, unreviewed, red, clean];

  it("selects exactly what each facet's label promises", () => {
    expect(all.filter((p) => prInFacet(p, "all"))).toHaveLength(4);
    expect(all.filter((p) => prInFacet(p, "drafts"))).toEqual([drafted]);
    expect(all.filter((p) => prInFacet(p, "failing"))).toEqual([red]);
    // A draft is not waiting on anyone, so it is not awaiting review.
    expect(all.filter((p) => prInFacet(p, "awaitingReview"))).toEqual([unreviewed]);
  });

  it("counts each facet with the same predicate the list filters by", () => {
    const counts = prFacetCounts(all);
    for (const facet of PR_FACETS) {
      expect(counts[facet], facet).toBe(all.filter((p) => prInFacet(p, facet)).length);
    }
  });
});

describe("filterPullRequests", () => {
  const rows = [pr(1, { title: "add caching" }), pr(2, { head_ref: "fix-login" })];

  it("matches number, title and both refs", () => {
    expect(filterPullRequests(rows, "all", "#1")).toHaveLength(1);
    expect(filterPullRequests(rows, "all", "caching")).toHaveLength(1);
    expect(filterPullRequests(rows, "all", "fix-login")).toHaveLength(1);
    expect(filterPullRequests(rows, "all", "main")).toHaveLength(2);
    expect(prSearchText(rows[0])).toContain("#1");
  });

  it("applies facet and query together, and keeps payload order", () => {
    const failing = [pr(9, { ci_status: "failure", title: "zzz" }), pr(1, { title: "aaa" })];
    expect(filterPullRequests(failing, "failing", "zzz").map((p) => p.number)).toEqual([9]);
    expect(filterPullRequests(failing, "all", "").map((p) => p.number)).toEqual([9, 1]);
  });

  it("treats a whitespace query as no query", () => {
    expect(filterPullRequests(rows, "all", "  ")).toHaveLength(2);
  });
});

describe("filterIssues", () => {
  const rows = [
    issue(1, { title: "crash on save", labels: ["bug"] }),
    issue(2, { title: "docs typo", author: "grace" }),
  ];

  it("matches number, title, author and labels", () => {
    expect(filterIssues(rows, "bug")).toHaveLength(1);
    expect(filterIssues(rows, "grace")).toHaveLength(1);
    expect(filterIssues(rows, "#2")).toHaveLength(1);
    expect(issueSearchText(rows[0])).toContain("bug");
  });

  it("returns everything for an empty query", () => {
    expect(filterIssues(rows, "")).toHaveLength(2);
  });
});

describe("runsOnBranch", () => {
  const runs = [run(1, { head_branch: "main" }), run(2, { head_branch: "feat" })];

  it("narrows to one branch", () => {
    expect(runsOnBranch(runs, "feat").map((r) => r.id)).toEqual([2]);
  });

  it("shows everything when there is no branch to narrow by", () => {
    // A detached HEAD has no branch name. Filtering on "" would blank the
    // list rather than turning the filter off.
    expect(runsOnBranch(runs, "")).toHaveLength(2);
    expect(runsOnBranch(runs, "   ")).toHaveLength(2);
  });
});

describe("relativeAge", () => {
  it("scales from minutes to years", () => {
    expect(relativeAge("2026-09-04T11:59:30Z", NOW)).toBe("just now");
    expect(relativeAge("2026-09-04T11:30:00Z", NOW)).toBe("30m ago");
    expect(relativeAge("2026-09-04T06:00:00Z", NOW)).toBe("6h ago");
    expect(relativeAge("2026-09-01T12:00:00Z", NOW)).toBe("3d ago");
    expect(relativeAge("2026-06-04T12:00:00Z", NOW)).toBe("3mo ago");
    expect(relativeAge("2024-09-04T12:00:00Z", NOW)).toBe("2y ago");
  });

  it("says nothing at all when there is no usable timestamp", () => {
    // A label reading "56y ago" for a missing timestamp is worse than no
    // label: it looks like data.
    expect(relativeAge("", NOW)).toBe("");
    expect(relativeAge(null, NOW)).toBe("");
    expect(relativeAge("not a date", NOW)).toBe("");
  });

  it("clamps a future timestamp rather than counting backwards", () => {
    expect(relativeAge("2027-01-01T00:00:00Z", NOW)).toBe("just now");
  });
});
