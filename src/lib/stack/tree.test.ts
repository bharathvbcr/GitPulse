import { describe, expect, it } from "vitest";
import type { BranchInfo } from "../branches/types";
import type { StackedBranchNode } from "./types";
import {
  cascadePlan,
  describeCascade,
  descendantsOf,
  rootlessBranches,
  stackBranchFacts,
  stackTreeRows,
} from "./tree";

function node(
  branch: string,
  parent: string | null,
  children: string[] = [],
  ahead = 1,
  tip = `tip-${branch}`,
): StackedBranchNode {
  return {
    branch_name: branch,
    tip_commit_id: tip,
    parent_branch_name: parent,
    child_branch_names: children,
    commit_count_ahead_of_parent: ahead,
  };
}

function branch(name: string, extra: Partial<BranchInfo> = {}): BranchInfo {
  return {
    name,
    is_current: false,
    is_remote: false,
    tip_commit_id: `tip-${name}`,
    ahead_count: 0,
    behind_count: 0,
    is_default: false,
    is_gone: false,
    last_commit_timestamp: 0,
    last_author: "",
    last_summary: "",
    commits_ahead_of_base: 0,
    commits_behind_base: 0,
    additions: 0,
    deletions: 0,
    files_changed: 0,
    ...extra,
  };
}

/** main <- feat-a <- feat-b, plus a second child of main. */
const CHAIN: StackedBranchNode[] = [
  node("main", null, ["feat-a", "zzz-side"], 0, "tip-main"),
  node("feat-a", "main", ["feat-b"]),
  node("feat-b", "feat-a"),
  node("zzz-side", "main"),
];

describe("stackTreeRows", () => {
  it("emits parents before their children, with depth", () => {
    const rows = stackTreeRows(CHAIN);
    expect(rows.map((r) => [r.node.branch_name, r.depth])).toEqual([
      ["main", 0],
      ["feat-a", 1],
      ["feat-b", 2],
      ["zzz-side", 1],
    ]);
  });

  it("marks the last child of each parent so the elbow connector is drawable", () => {
    const rows = stackTreeRows(CHAIN);
    const byName = new Map(rows.map((r) => [r.node.branch_name, r]));
    expect(byName.get("feat-a")?.isLast).toBe(false);
    expect(byName.get("zzz-side")?.isLast).toBe(true);
    expect(byName.get("feat-a")?.hasChildren).toBe(true);
    expect(byName.get("feat-b")?.hasChildren).toBe(false);
  });

  it("orders siblings by name so the same repository draws the same tree", () => {
    const shuffled = [CHAIN[3], CHAIN[2], CHAIN[0], CHAIN[1]];
    expect(stackTreeRows(shuffled)).toEqual(stackTreeRows(CHAIN));
  });

  it("emits every node exactly once even when parent pointers form a cycle", () => {
    // The backend guards its own breadcrumb walk against this; the renderer
    // must not be the place a malformed payload turns into a hang.
    const cyclic = [
      node("a", "b", ["b"]),
      node("b", "a", ["a"]),
      node("main", null),
    ];
    const rows = stackTreeRows(cyclic);
    expect(rows.map((r) => r.node.branch_name).sort()).toEqual(["a", "b", "main"]);
  });

  it("treats a branch whose named parent is absent as a root, never dropping it", () => {
    const orphaned = [node("feat", "ghost"), node("main", null)];
    const rows = stackTreeRows(orphaned);
    expect(rows.map((r) => r.node.branch_name).sort()).toEqual(["feat", "main"]);
    expect(rows.every((r) => r.depth === 0)).toBe(true);
  });

  it("ignores a child list entry that does not point back at this parent", () => {
    // Only the parent pointer decides parentage; a stale child name must not
    // reparent a branch under a node it never named.
    const disagreeing = [node("main", null, ["feat"]), node("feat", "other"), node("other", null)];
    const rows = stackTreeRows(disagreeing);
    const feat = rows.find((r) => r.node.branch_name === "feat");
    expect(feat?.depth).toBe(1);
    const main = rows.find((r) => r.node.branch_name === "main");
    expect(main?.hasChildren).toBe(false);
  });
});

describe("descendantsOf", () => {
  it("returns everything stacked above a branch, nearest first", () => {
    expect(descendantsOf(CHAIN, "main").map((n) => n.branch_name)).toEqual([
      "feat-a",
      "zzz-side",
      "feat-b",
    ]);
    expect(descendantsOf(CHAIN, "feat-a").map((n) => n.branch_name)).toEqual(["feat-b"]);
    expect(descendantsOf(CHAIN, "feat-b")).toEqual([]);
  });

  it("terminates on a cycle rather than walking it forever", () => {
    const cyclic = [node("a", "b", ["b"]), node("b", "a", ["a"])];
    expect(descendantsOf(cyclic, "a").map((n) => n.branch_name)).toEqual(["b"]);
  });
});

describe("cascadePlan", () => {
  it("replays the start branch from a computed fork point and each child from its recorded parent tip", () => {
    // The reason the plan exists: once feat-a is rebased, the commit feat-b
    // was cut from is unreachable from feat-a, so nothing computed after the
    // first rewrite can recover it.
    expect(cascadePlan(CHAIN, "feat-a")).toEqual([
      { branch: "feat-a", onto: "main", forkPoint: null },
      { branch: "feat-b", onto: "feat-a", forkPoint: "tip-feat-a" },
    ]);
  });

  it("plans a whole subtree in parent-before-child order", () => {
    const deep = [
      node("main", null, ["a"], 0, "tip-main"),
      node("a", "main", ["b"]),
      node("b", "a", ["c"]),
      node("c", "b"),
    ];
    const plan = cascadePlan(deep, "a");
    expect(plan.map((s) => s.branch)).toEqual(["a", "b", "c"]);
    expect(plan.map((s) => s.forkPoint)).toEqual([null, "tip-a", "tip-b"]);
  });

  it("offers nothing for a branch with no parent to be replayed onto", () => {
    // A root of the repository is not a stack; a button that rebases it onto
    // nothing is a control that cannot do anything.
    expect(cascadePlan(CHAIN, "main")).toEqual([]);
    expect(cascadePlan(CHAIN, "unknown")).toEqual([]);
  });
});

describe("describeCascade", () => {
  it("names every branch the rewrite will touch", () => {
    const text = describeCascade(cascadePlan(CHAIN, "feat-a"));
    expect(text).toContain("feat-a");
    expect(text).toContain("feat-b");
    expect(text).toContain("onto main");
    expect(text).toContain("rewrites");
  });

  it("is empty for an empty plan, so no confirmation can be raised for a no-op", () => {
    expect(describeCascade([])).toBe("");
  });

  it("reads as one sentence for a single branch", () => {
    expect(describeCascade([{ branch: "feat-b", onto: "feat-a", forkPoint: null }])).toBe(
      "Rebase feat-b onto feat-a. This rewrites its commits.",
    );
  });

  it("caps the list rather than pasting a hundred names into a dialog", () => {
    const many = Array.from({ length: 9 }, (_, i) => ({
      branch: `b${i}`,
      onto: "main",
      forkPoint: null,
    }));
    const text = describeCascade(many);
    expect(text).toContain("Rebase 9 branches onto main");
    expect(text).toContain("…");
    expect(text).not.toContain("b7");
  });
});

describe("stackBranchFacts", () => {
  it("reports nothing rather than confident zeroes for a branch the list has not got", () => {
    expect(stackBranchFacts("feat-a", [])).toBeNull();
  });

  it("carries the base comparison and the tracking state", () => {
    const facts = stackBranchFacts("feat-a", [
      branch("feat-a", {
        commits_behind_base: 4,
        compared_to: "main",
        upstream: "origin/feat-a",
        ahead_count: 2,
        behind_count: 1,
        last_author: "ada",
      }),
    ]);
    expect(facts).toMatchObject({
      behindBase: 4,
      comparedTo: "main",
      upstream: { name: "origin/feat-a", ahead: 2, behind: 1, gone: false },
      lastAuthor: "ada",
    });
  });

  it("says untracked rather than zero-ahead-zero-behind for a branch with no upstream", () => {
    const facts = stackBranchFacts("feat-a", [branch("feat-a")]);
    expect(facts?.upstream).toBeNull();
  });

  it("never reads a remote branch of the same name", () => {
    const facts = stackBranchFacts("feat-a", [
      branch("feat-a", { is_remote: true, commits_behind_base: 9 }),
    ]);
    expect(facts).toBeNull();
  });
});

describe("rootlessBranches", () => {
  it("names the local branches the hierarchy placed nowhere", () => {
    const branches = [
      branch("main", { is_default: true }),
      branch("feat-a"),
      branch("feat-b"),
      branch("drifted"),
      branch("origin/drifted", { is_remote: true }),
    ];
    expect(rootlessBranches(CHAIN, branches, "main")).toEqual(["drifted"]);
  });

  it("does not report the default branch as unplaced", () => {
    expect(rootlessBranches([], [branch("main")], "main")).toEqual([]);
  });

  it("reports every non-default branch when the hierarchy is empty", () => {
    const branches = [branch("main"), branch("a"), branch("b")];
    expect(rootlessBranches([], branches, "main")).toEqual(["a", "b"]);
  });
});
