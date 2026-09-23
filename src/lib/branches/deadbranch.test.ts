import { describe, expect, it } from "vitest";
import {
  filterStaleBranches,
  formatBackupTimestamp,
  formatBranchAge,
  severityBadge,
} from "./deadbranch";
import type { StaleBranchInfo } from "./types";

function sampleBranch(overrides: Partial<StaleBranchInfo> = {}): StaleBranchInfo {
  return {
    name: "feature/auth",
    short_name: "feature/auth",
    age_days: 45,
    severity: "moderate",
    is_merged: false,
    merged_by_tree: false,
    is_remote: false,
    is_protected: false,
    is_wip: false,
    is_current_or_worktree: false,
    last_commit_sha: "abcdef1234567890",
    last_commit_timestamp: 1700000000,
    last_author: "Developer",
    last_summary: "Add auth login flow",
    ...overrides,
  };
}

describe("deadbranch frontend helpers", () => {
  it("formats branch ages properly", () => {
    expect(formatBranchAge(0)).toBe("today");
    expect(formatBranchAge(1)).toBe("1 day ago");
    expect(formatBranchAge(15)).toBe("15 days ago");
    expect(formatBranchAge(35)).toBe("1 month ago");
    expect(formatBranchAge(95)).toBe("3 months ago");
    expect(formatBranchAge(400)).toBe("1 year ago");
    expect(formatBranchAge(800)).toBe("2 years ago");
  });

  it("assigns appropriate severity badges", () => {
    const fresh = severityBadge("fresh");
    expect(fresh.label).toBe("Fresh");
    expect(fresh.textClass).toContain("text-emerald");

    const moderate = severityBadge("moderate");
    expect(moderate.label).toBe("Moderate");
    expect(moderate.textClass).toContain("text-amber");

    const stale = severityBadge("stale");
    expect(stale.label).toBe("Stale");
    expect(stale.textClass).toContain("text-rose");
  });

  it("formats backup timestamps into human readable string", () => {
    expect(formatBackupTimestamp(0)).toBe("Unknown");
    const ts = 1700000000;
    const formatted = formatBackupTimestamp(ts);
    expect(formatted.length).toBeGreaterThan(5);
  });

  it("filters branches by query, minimum days, mergedOnly, and localOnly", () => {
    const branches: StaleBranchInfo[] = [
      sampleBranch({ name: "feature/old-1", age_days: 10, is_merged: false }),
      sampleBranch({ name: "feature/old-2", age_days: 40, is_merged: true }),
      sampleBranch({ name: "feature/old-3", age_days: 100, is_merged: false }),
      sampleBranch({ name: "origin/remote-1", is_remote: true, age_days: 50, is_merged: true }),
    ];

    // Filter by query
    const queried = filterStaleBranches(branches, "old-2", 0, false, false);
    expect(queried).toHaveLength(1);
    expect(queried[0].name).toBe("feature/old-2");

    // Filter by minimum age
    const aged = filterStaleBranches(branches, "", 30, false, false);
    expect(aged.map((b) => b.name)).toEqual([
      "feature/old-2",
      "feature/old-3",
      "origin/remote-1",
    ]);

    // Filter by mergedOnly
    const merged = filterStaleBranches(branches, "", 0, true, false);
    expect(merged.map((b) => b.name)).toEqual([
      "feature/old-2",
      "origin/remote-1",
    ]);

    // Filter by localOnly
    const local = filterStaleBranches(branches, "", 0, false, true);
    expect(local.map((b) => b.name)).toEqual([
      "feature/old-1",
      "feature/old-2",
      "feature/old-3",
    ]);
  });
});
