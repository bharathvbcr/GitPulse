import { describe, expect, it } from "vitest";
import {
  branchLeafName,
  filterBranchSections,
  fuzzyMatch,
  groupBranches,
  highlightMatches,
  isStaleBranch,
  localNameFor,
} from "./groupBranches";
import type { BranchFolder, BranchInfo, BranchSection } from "./types";

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

describe("fuzzyMatch and highlightMatches", () => {
  it("matches substrings and subsequences", () => {
    expect(fuzzyMatch("", "feat/auth")).toBe(true);
    expect(fuzzyMatch("auth", "feat/auth")).toBe(true);
    expect(fuzzyMatch("fta", "feat/auth")).toBe(true);
    expect(fuzzyMatch("xyz", "feat/auth")).toBe(false);
  });

  it("chunks text for search highlight accurately", () => {
    expect(highlightMatches("feature/auth", "auth")).toEqual([
      { text: "feature/", matched: false },
      { text: "auth", matched: true },
    ]);
    expect(highlightMatches("main", "")).toEqual([{ text: "main", matched: false }]);
    expect(highlightMatches("feature/login", "ftr")).toEqual([
      { text: "f", matched: true },
      { text: "ea", matched: false },
      { text: "t", matched: true },
      { text: "u", matched: false },
      { text: "r", matched: true },
      { text: "e/login", matched: false },
    ]);
    expect(highlightMatches("main", "zzz")).toEqual([{ text: "main", matched: false }]);
    expect(highlightMatches("", "x")).toEqual([{ text: "", matched: false }]);
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

describe("groupBranches at scale", () => {
  it("groups and filters 20k mixed-depth branches without quadratic blowup", () => {
    const branches: BranchInfo[] = [];
    for (let i = 0; i < 20_000; i++) {
      // Cycle nesting depths so folder fan-out and reuse are both exercised.
      const depth = (i % 4) + 1;
      const folders = Array.from({ length: depth }, (_, d) => `lvl${d}`).join("/");
      const isRemote = i % 5 === 4;
      branches.push(
        branch({
          name: `${folders}/br${i}`,
          is_remote: isRemote,
          remote_name: isRemote ? "origin" : null,
        })
      );
    }

    const t0 = performance.now();
    const sections = groupBranches(branches);
    const filtered = filterBranchSections(sections, "br19999");
    const elapsedMs = performance.now() - t0;

    // Generous bound purely to catch accidental quadratic behavior.
    expect(elapsedMs).toBeLessThan(2000);

    // Correctness spot checks: every branch lands in exactly one section.
    const totalGrouped = sections.reduce((n, s) => n + s.branchCount, 0);
    expect(totalGrouped).toBe(20_000);
    const local = sections.find((s) => s.id === "local")!;
    const origin = sections.find((s) => s.id === "remote:origin")!;
    expect(local.branchCount).toBe(16_000);
    expect(origin.branchCount).toBe(4_000);

    // "br19999" substring-matches exactly one branch (i=19999, a remote,
    // depth-4 branch); subsequence matching cannot reach any other leaf
    // because no other name carries five ordered nines after its last "1".
    expect(filtered).toHaveLength(1);
    expect(filtered[0].id).toBe("remote:origin");
    expect(filtered[0].branchCount).toBe(1);
    let deepest: BranchSection | BranchFolder = filtered[0];
    for (const label of ["lvl0", "lvl1", "lvl2", "lvl3"]) {
      const next = deepest.folders.find((f) => f.label === label);
      expect(next).toBeDefined();
      deepest = next!;
    }
    expect(deepest.branches.map((b) => b.name)).toEqual(["lvl0/lvl1/lvl2/lvl3/br19999"]);
  });

  it("supports pinned branches and tab filtering", () => {
    const branches = [
      branch({ name: "main", is_current: true }),
      branch({ name: "feat/auth", last_commit_timestamp: 1_800_000_000 }),
      branch({ name: "legacy/old", last_commit_timestamp: 1_500_000_000 }),
      branch({ name: "origin/feat/auth", is_remote: true, remote_name: "origin" }),
    ];
    const tags = [{ name: "v1.0.0", commit_id: "aaa" }];
    const pinned = new Set(["feat/auth"]);

    const sections = groupBranches(branches, tags, pinned);
    expect(sections.map((s) => s.id)).toEqual(["pinned", "local", "remote:origin", "tags"]);
    expect(sections[0].branches.map((b) => b.name)).toEqual(["feat/auth"]);

    // Test tab filtering
    const localTab = filterBranchSections(sections, "", "local");
    expect(localTab.map((s) => s.id)).toEqual(["pinned", "local"]);

    const remoteTab = filterBranchSections(sections, "", "remote");
    expect(remoteTab.map((s) => s.id)).toEqual(["remote:origin"]);

    const tagsTab = filterBranchSections(sections, "", "tags");
    expect(tagsTab.map((s) => s.id)).toEqual(["tags"]);

    const pinnedTab = filterBranchSections(sections, "", "pinned");
    expect(pinnedTab.map((s) => s.id)).toEqual(["pinned"]);

    const now = 1_800_000_000;
    const activeTab = filterBranchSections(sections, "", "active", now);
    const staleTab = filterBranchSections(sections, "", "stale", now);
    expect(activeTab.some((s) => s.branchCount > 0)).toBe(true);
    expect(staleTab.some((s) => s.folders.some((f) => f.branches.some((b) => b.name === "legacy/old")))).toBe(true);
  });
});
