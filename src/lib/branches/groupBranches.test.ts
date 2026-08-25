import { describe, expect, it } from "vitest";
import {
  branchLeafName,
  filterBranchSections,
  fuzzyMatch,
  groupBranches,
  isStaleBranch,
  localNameFor,
} from "./groupBranches";
import type { BranchInfo } from "./types";

function branch(partial: Partial<BranchInfo> & Pick<BranchInfo, "name">): BranchInfo {
  return {
    is_current: false,
    is_remote: false,
    tip_commit_id: "abc",
    ahead_count: 0,
    behind_count: 0,
    is_default: false,
    is_gone: false,
    last_commit_timestamp: 1,
    last_author: "Ada",
    last_summary: "feat: work",
    commits_ahead_of_base: 0,
    commits_behind_base: 0,
    additions: 0,
    deletions: 0,
    files_changed: 0,
    ...partial,
  };
}

describe("fuzzyMatch", () => {
  it("matches substrings and subsequences", () => {
    expect(fuzzyMatch("", "feat/auth")).toBe(true);
    expect(fuzzyMatch("auth", "feat/auth")).toBe(true);
    expect(fuzzyMatch("fta", "feat/auth")).toBe(true);
    expect(fuzzyMatch("xyz", "feat/auth")).toBe(false);
  });
});

describe("groupBranches", () => {
  it("nests slash prefixes under Local and groups remotes by origin", () => {
    const sections = groupBranches([
      branch({ name: "main", is_current: true, is_default: true }),
      branch({ name: "feat/auth" }),
      branch({ name: "feat/payments" }),
      branch({ name: "feat/auth/oauth" }),
      branch({ name: "bugfix/login" }),
      branch({
        name: "origin/main",
        is_remote: true,
        remote_name: "origin",
      }),
      branch({
        name: "origin/feat/auth",
        is_remote: true,
        remote_name: "origin",
      }),
    ]);

    expect(sections.map((s) => s.id)).toEqual(["local", "remote:origin"]);
    const local = sections[0];
    expect(local.branches.map((b) => b.name)).toEqual(["main"]);
    expect(local.folders.map((f) => f.label).sort()).toEqual(["bugfix", "feat"]);
    const feat = local.folders.find((f) => f.label === "feat")!;
    expect(feat.branches.map((b) => b.name).sort()).toEqual(["feat/auth", "feat/payments"]);
    expect(feat.folders).toHaveLength(1);
    expect(feat.folders[0].label).toBe("auth");
    expect(feat.folders[0].branches.map((b) => b.name)).toEqual(["feat/auth/oauth"]);
    expect(local.branchCount).toBe(5);

    const origin = sections[1];
    expect(origin.kind).toBe("remote");
    expect(origin.branches.map((b) => b.name)).toEqual(["origin/main"]);
    expect(origin.folders[0].label).toBe("feat");
    expect(origin.folders[0].branches.map((b) => b.name)).toEqual(["origin/feat/auth"]);
  });

  it("puts tags in their own section", () => {
    const sections = groupBranches(
      [branch({ name: "main", is_default: true })],
      [{ name: "v1.0.0", commit_id: "aaa" }, { name: "v0.9.0", commit_id: "bbb" }]
    );
    const tags = sections.find((s) => s.kind === "tags")!;
    expect(tags.tags.map((t) => t.name)).toEqual(["v1.0.0", "v0.9.0"]);
  });

  it("filters folders and tags by query while keeping ancestors", () => {
    const grouped = groupBranches([
      branch({ name: "main", is_default: true }),
      branch({ name: "feat/auth" }),
      branch({ name: "bugfix/login" }),
    ]);
    const filtered = filterBranchSections(grouped, "auth");
    expect(filtered).toHaveLength(1);
    expect(filtered[0].branches).toHaveLength(0);
    expect(filtered[0].folders[0].label).toBe("feat");
    expect(filtered[0].folders[0].branches.map((b) => b.name)).toEqual(["feat/auth"]);
  });
});

describe("branch name helpers", () => {
  it("strips remote prefixes and returns the leaf", () => {
    const remote = branch({
      name: "origin/feat/auth",
      is_remote: true,
      remote_name: "origin",
    });
    expect(localNameFor(remote)).toBe("feat/auth");
    expect(branchLeafName(remote)).toBe("auth");
    expect(branchLeafName(branch({ name: "main" }))).toBe("main");
  });

  it("marks branches older than 90 days as stale", () => {
    const now = 1_800_000_000;
    expect(isStaleBranch(now - 10 * 86400, now)).toBe(false);
    expect(isStaleBranch(now - 100 * 86400, now)).toBe(true);
    expect(isStaleBranch(0, now)).toBe(false);
  });
});
