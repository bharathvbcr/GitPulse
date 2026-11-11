import { describe, expect, it } from "vitest";
import type { CollisionRisk } from "../insights/types";
import type { PullRequestInfo } from "../github/types";
import {
  branchPullRequest,
  compactAge,
  fetchGlance,
  overlapGlance,
  pullRequestGlance,
  stashGlance,
  STALE_FETCH_MS,
} from "./viewBarGlance";

const NOW = 1_800_000_000_000;
const MIN = 60_000;
const HOUR = 60 * MIN;

function pr(over: Partial<PullRequestInfo> = {}): PullRequestInfo {
  return {
    number: 42, title: "Cache the graph", state: "OPEN", head_ref: "feat/cache", base_ref: "main",
    url: "https://example.test/pr/42", is_draft: false, ci_status: "success", created_at: "", updated_at: "",
    review_decision: "", first_review_at: "", ...over,
  };
}

function collisions(over: Partial<CollisionRisk> = {}): CollisionRisk {
  return {
    ok: true, error: "", overlapping_files: 0, worktrees_involved: 0, scanned_worktrees: 3, unscanned_worktrees: 0,
    failed_worktrees: 0, truncated: false, items: [], shared_worktree_files: 0, shared_worktrees: [], sessions_ok: true,
    sessions_error: "", ...over,
  };
}

describe("compactAge", () => {
  it("says the age in the fewest characters, rounding down so it never overstates freshness", () => {
    expect(compactAge(0)).toBe("now");
    expect(compactAge(59_999)).toBe("now");
    expect(compactAge(MIN)).toBe("1m");
    expect(compactAge(59 * MIN + 59_000)).toBe("59m");
    expect(compactAge(HOUR)).toBe("1h");
    expect(compactAge(47 * HOUR)).toBe("47h");
    expect(compactAge(48 * HOUR)).toBe("2d");
  });
});

describe("fetchGlance", () => {
  it("shows nothing before a read (undefined) or with no fetch recorded (null)", () => {
    expect(fetchGlance(undefined, NOW)).toBeNull();
    expect(fetchGlance(null, NOW)).toBeNull();
    expect(fetchGlance(0, NOW)).toBeNull();
    expect(fetchGlance(Number.NaN, NOW)).toBeNull();
  });

  it("is neutral under a day and warns from a day, its label saying how long", () => {
    const fresh = fetchGlance(NOW - 3 * HOUR, NOW);
    expect(fresh).toMatchObject({ text: "3h", tone: "neutral" });
    expect(fresh?.label).toBe("Last fetched 3h ago. Fetch now");
    expect(fetchGlance(NOW - STALE_FETCH_MS + MIN, NOW)?.tone).toBe("neutral");
    const stale = fetchGlance(NOW - STALE_FETCH_MS, NOW);
    expect(stale).toMatchObject({ text: "24h", tone: "warn" });
    expect(stale?.label).toBe("Last fetched 1d ago. Fetch now");
  });

  it("reads a fetch stamped after the clock (a tick behind) as just now, not a negative age", () => {
    expect(fetchGlance(NOW + 30_000, NOW)?.text).toBe("now");
  });
});

describe("stashGlance", () => {
  it("shows nothing for an empty stash or one whose read failed", () => {
    expect(stashGlance({ entries: 0, truncated: false, failed: false })).toBeNull();
    expect(stashGlance({ entries: 3, truncated: false, failed: true })).toBeNull();
  });

  it("counts entries, and a capped list as a floor", () => {
    expect(stashGlance({ entries: 1, truncated: false, failed: false })).toMatchObject({ text: "1", label: "1 stash entry" });
    expect(stashGlance({ entries: 3, truncated: false, failed: false })?.label).toBe("3 stash entries");
    const capped = stashGlance({ entries: 50, truncated: true, failed: false });
    expect(capped?.text).toBe("50+");
    expect(capped?.label).toContain("At least 50 stash entries");
  });
});

describe("branchPullRequest", () => {
  it("finds the open pull request whose head is this branch", () => {
    const prs = [pr({ number: 1, head_ref: "other" }), pr({ number: 2 })];
    expect(branchPullRequest(prs, "feat/cache")?.number).toBe(2);
  });

  it("finds none without a branch (detached HEAD), for a fork's same-named branch, or for a closed one", () => {
    expect(branchPullRequest([pr()], null)).toBeNull();
    expect(branchPullRequest([pr({ is_cross_repository: true })], "feat/cache")).toBeNull();
    expect(branchPullRequest([pr({ state: "MERGED" }), pr({ state: "closed" })], "feat/cache")).toBeNull();
  });
});

describe("pullRequestGlance", () => {
  it("names the number, its checks as a tone, and the age of the read it comes from", () => {
    const glance = pullRequestGlance(pr(), NOW - 5 * MIN, NOW);
    expect(glance).toMatchObject({ text: "#42", tone: "ok" });
    expect(glance?.label).toBe("Pull request #42, checks passed, as of 5m ago: Cache the graph");
  });

  it("maps each CI state, and an unrecognised one to unknown words, never to passed", () => {
    expect(pullRequestGlance(pr({ ci_status: "failure" }), NOW, NOW)?.tone).toBe("danger");
    expect(pullRequestGlance(pr({ ci_status: "pending" }), NOW, NOW)?.tone).toBe("warn");
    expect(pullRequestGlance(pr({ ci_status: "none" }), NOW, NOW)?.label).toContain("no checks");
    const odd = pullRequestGlance(pr({ ci_status: "unknown" }), NOW, NOW);
    expect(odd?.tone).toBe("neutral");
    expect(odd?.label).toContain("checks unknown");
  });

  it("says draft, and shows nothing without a pull request", () => {
    expect(pullRequestGlance(pr({ is_draft: true }), NOW, NOW)?.label).toMatch(/^Draft pull request #42/);
    expect(pullRequestGlance(null, NOW, NOW)).toBeNull();
  });
});

describe("overlapGlance", () => {
  it("shows nothing for no facet, a failed scan, or no overlap", () => {
    expect(overlapGlance(null, NOW, NOW)).toBeNull();
    expect(overlapGlance(undefined, NOW, NOW)).toBeNull();
    expect(overlapGlance(collisions({ ok: false, error: "boom", overlapping_files: 4 }), NOW, NOW)).toBeNull();
    expect(overlapGlance(collisions(), NOW, NOW)).toBeNull();
  });

  it("counts files changed in more than one worktree, names the age of the read, and opens Agents", () => {
    const glance = overlapGlance(collisions({ overlapping_files: 2, worktrees_involved: 3 }), NOW - 20 * MIN, NOW);
    expect(glance).toMatchObject({ text: "2", tone: "warn" });
    expect(glance?.label).toBe("2 files changed in more than one worktree across 3 worktrees, as of 20m ago. Open Agents");
    // No sweep has finished: nothing to date it by, so no age is claimed.
    expect(overlapGlance(collisions({ overlapping_files: 1, worktrees_involved: 2 }), null, NOW)?.label)
      .toBe("1 file changed in more than one worktree across 2 worktrees. Open Agents");
  });

  it("is a floor when the scan was capped, skipped worktrees or lost one", () => {
    for (const partial of [{ truncated: true }, { unscanned_worktrees: 1 }, { failed_worktrees: 1 }]) {
      const glance = overlapGlance(collisions({ overlapping_files: 1, worktrees_involved: 2, ...partial }), NOW, NOW);
      expect(glance?.text).toBe("1+");
      expect(glance?.label).toMatch(/^At least 1 file /);
    }
  });
});
