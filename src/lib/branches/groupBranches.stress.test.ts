import { describe, expect, it } from "vitest";
import {
  branchLeafName,
  filterBranchSections,
  fuzzyMatch,
  groupBranches,
  isStaleBranch,
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

// Mirrors groupBranches.ts's private STALE_SECONDS (90 days).
const STALE_SECONDS = 90 * 24 * 60 * 60;

describe("groupBranches stress: 5,000 branches across 50 namespaces", () => {
  it("groups and filters without blowup, with deterministic ordering", () => {
    const branches: BranchInfo[] = [];
    for (let i = 0; i < 5_000; i += 1) {
      const ns = `ns${String(i % 50).padStart(2, "0")}`;
      branches.push(branch({ name: `${ns}/br-${i}` }));
    }

    const startedAt = performance.now();
    const sections = groupBranches(branches);
    // Query keeps exactly the one namespace folder: "ns07/" can only
    // include-match the folder label, and the subsequence scan cannot spell
    // it out of any other namespace (the "/" pins everything before it to
    // the two-digit "07").
    const filtered = filterBranchSections(sections, "ns07/");
    const elapsedMs = performance.now() - startedAt;

    expect(elapsedMs).toBeLessThan(2_000);

    // Correctness spot checks.
    const local = sections.find((s) => s.id === "local")!;
    expect(sections).toHaveLength(1);
    expect(local.branchCount).toBe(5_000);
    expect(local.folders).toHaveLength(50);
    const ns07 = local.folders.find((f) => f.label === "ns07")!;
    expect(ns07.branches).toHaveLength(100);
    expect(ns07.branches.map((b) => b.name)).toContain("ns07/br-7");

    // Query keeps exactly the one namespace folder, with all its branches.
    expect(filtered).toHaveLength(1);
    expect(filtered[0].folders).toHaveLength(1);
    expect(filtered[0].branchCount).toBe(100);

    // Determinism: folder labels sorted, and a second run is identical.
    const labels = local.folders.map((f) => f.label);
    expect([...labels].sort((a, b) => a.localeCompare(b))).toEqual(labels);
    expect(JSON.stringify(groupBranches(branches))).toBe(JSON.stringify(sections));
  });
});

describe("groupBranches stress: pathological names", () => {
  it("nests a single branch ten segments deep", () => {
    const sections = groupBranches([branch({ name: "a/b/c/d/e/f/g/h/i/j" })]);
    const local = sections[0];
    expect(local.branches).toHaveLength(0);
    let cursor: ReturnType<typeof groupBranches>[number] | ReturnType<typeof groupBranches>[number]["folders"][number] = local;
    const labels = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    for (const label of labels) {
      expect(cursor.folders).toHaveLength(1);
      cursor = cursor.folders[0];
      expect(cursor.label).toBe(label);
    }
    expect(cursor.branches.map((b) => b.name)).toEqual(["a/b/c/d/e/f/g/h/i/j"]);
    expect(local.branchCount).toBe(1);
  });

  it("counts duplicate branch names without corrupting totals", () => {
    // Defensible-but-surprising: no dedup at grouping level (git never yields
    // duplicates upstream), so both entries are counted. Pinned as contract.
    const dup = branch({ name: "feat/x" });
    const sections = groupBranches([dup, { ...dup }, branch({ name: "main" })]);
    const local = sections[0];
    expect(local.branchCount).toBe(3);
    const feat = local.folders.find((f) => f.label === "feat")!;
    expect(feat.branches).toHaveLength(2);
  });

  it("keeps names differing only by case in distinct folders", () => {
    const sections = groupBranches([
      branch({ name: "feat/Auth" }),
      branch({ name: "feat/auth" }),
      branch({ name: "Feature/One" }),
    ]);
    const local = sections[0];
    expect(local.branchCount).toBe(3);
    const labels = local.folders.map((f) => f.label).sort();
    expect(labels).toEqual(["Feature", "feat"]);
    const feat = local.folders.find((f) => f.label === "feat")!;
    expect(feat.branches.map((b) => b.name).sort()).toEqual(["feat/Auth", "feat/auth"]);
  });

  it("treats a slash-only name as a bare branch, not a folder path", () => {
    // splitPath filters empty parts, so "//" has folders=[] and lands
    // directly in section.branches under its raw name. Pinned.
    const sections = groupBranches([branch({ name: "//" })]);
    const local = sections[0];
    expect(local.folders).toHaveLength(0);
    expect(local.branches.map((b) => b.name)).toEqual(["//"]);
    expect(local.branchCount).toBe(1);
    expect(branchLeafName(branch({ name: "//" }))).toBe("//");
  });

  it("treats a trailing-slash name as a bare branch too", () => {
    const sections = groupBranches([branch({ name: "feat/" })]);
    const local = sections[0];
    expect(local.folders).toHaveLength(0);
    expect(local.branches.map((b) => b.name)).toEqual(["feat/"]);
    expect(local.branchCount).toBe(1);
  });
});

describe("fuzzyMatch stress: regex metacharacters are literal", () => {
  const cases: Array<[string, string, boolean]> = [
    [".", "v1.2.3", true],
    [".", "plain-name", false],
    ["*", "release-*", true],
    ["*", "release-star", false],
    ["+", "c++-runtime", true],
    ["?", "what?next", true],
    ["?", "questionable", false],
    ["[wip]", "[wip] topic", true],
    ["[wip]", "wip topic", false],
    ["(", "fn(main)", true],
    [")", "fn(main)", true],
    ["v12", "v1.2.*", true],
    [".*+?[]()", ".*+?[]()", true],
    [".*+?[]()", "main", false],
    ["...", "a.b.c.d", true],
  ];

  it("never throws and always answers substring/subsequence semantics", () => {
    for (const [query, text, expected] of cases) {
      expect(fuzzyMatch(query, text)).toBe(expected);
    }
  });

  it("filters a mixed-metachar corpus without throwing", () => {
    const sections = groupBranches([
      branch({ name: "v1.2.*" }),
      branch({ name: "c++?" }),
      branch({ name: "[wip]-topic" }),
      branch({ name: "main" }),
    ]);
    for (const query of [".", "*", "+", "?", "[", "]", "(", ")", ".*"]) {
      expect(() => filterBranchSections(sections, query)).not.toThrow();
    }
    const dotted = filterBranchSections(sections, ".");
    expect(dotted[0].branches.map((b) => b.name)).toEqual(["v1.2.*"]);
  });
});

describe("filterBranchSections stress: ordering determinism", () => {
  it("preserves local → remotes → tags ordering across filters", () => {
    const grouped = groupBranches(
      [
        branch({ name: "zeta-bravo" }),
        branch({ name: "alpha-bravo" }),
        branch({
          name: "origin/zeta-bravo",
          is_remote: true,
          remote_name: "origin",
        }),
        branch({ name: "upstream/alpha-x", is_remote: true, remote_name: "upstream" }),
      ],
      [
        { name: "v2.0", commit_id: "a" },
        { name: "v1.0", commit_id: "b" },
      ]
    );
    expect(grouped.map((s) => s.id)).toEqual(["local", "remote:origin", "remote:upstream", "tags"]);

    // A query hitting every section must not reorder them; repeated calls
    // return identical structures.
    const once = filterBranchSections(grouped, "bravo");
    const twice = filterBranchSections(grouped, "bravo");
    expect(once.map((s) => s.id)).toEqual(["local", "remote:origin"]);
    expect(JSON.stringify(once)).toBe(JSON.stringify(twice));

    // Tags drop out when nothing matches; surviving order still holds.
    const tagsDropped = filterBranchSections(grouped, "bravo-nope");
    expect(tagsDropped).toHaveLength(0);
  });
});

describe("isStaleBranch stress: threshold boundaries", () => {
  const now = 1_800_000_000;

  it("uses strict greater-than at exactly 90 days", () => {
    expect(isStaleBranch(now - STALE_SECONDS, now)).toBe(false);
    expect(isStaleBranch(now - (STALE_SECONDS + 1), now)).toBe(true);
    expect(isStaleBranch(now - (STALE_SECONDS - 1), now)).toBe(false);
  });

  it("never marks the zero sentinel stale regardless of age", () => {
    expect(isStaleBranch(0, now)).toBe(false);
    expect(isStaleBranch(0, now + STALE_SECONDS * 10)).toBe(false);
  });
});
