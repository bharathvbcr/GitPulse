import { describe, expect, it } from "vitest";
import { gapGroupTitle, gapsBlockReading, groupGaps, type GapNote } from "./gapGroups";

const repo = (label: string, reason = "The insights read returned no snapshot.", kind: GapNote["kind"] = "failed"): GapNote => ({
  repoPath: `/code/${label}`,
  label,
  kind,
  reason,
});

describe("groupGaps", () => {
  it("folds the same failure across repositories into one group that keeps every name", () => {
    const labels = ["Other", "GitPulse", "alpha", "beta", "gamma"];
    const groups = groupGaps(labels.map((label) => repo(label)));
    expect(groups).toHaveLength(1);
    expect(groups[0].labels).toEqual(labels);
    expect(gapGroupTitle(groups[0])).toBe("5 repositories");
  });

  it("keeps different reasons, and different kinds of the same reason, apart, in first-seen order", () => {
    const groups = groupGaps([
      repo("alpha"),
      repo("beta", "Worktrees could not be listed."),
      repo("gamma"),
      repo("delta", "The insights read returned no snapshot.", "partial"),
    ]);
    expect(groups.map((group) => group.labels)).toEqual([["alpha", "gamma"], ["beta"], ["delta"]]);
    expect(gapGroupTitle(groups[1])).toBe("beta");
  });

  it("never merges notes that belong to no repository, even when their wording matches", () => {
    const tasks: GapNote = { repoPath: "", label: "Tasks", kind: "failed", reason: "Task storage returned an invalid response." };
    const groups = groupGaps([tasks, { ...tasks, label: "Repositories" }]);
    expect(groups).toHaveLength(2);
    expect(groups.every((group) => !group.repositories)).toBe(true);
  });

  it("returns nothing for no notes", () => {
    expect(groupGaps([])).toEqual([]);
  });
});

describe("gapsBlockReading", () => {
  it("is true only for a failed or skipped read", () => {
    expect(gapsBlockReading(groupGaps([repo("a", "capped", "partial")]))).toBe(false);
    expect(gapsBlockReading(groupGaps([repo("a", "late", "unread")]))).toBe(false);
    expect(gapsBlockReading(groupGaps([repo("a", "late", "skipped")]))).toBe(true);
    expect(gapsBlockReading(groupGaps([repo("a")]))).toBe(true);
  });
});
