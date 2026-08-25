import { describe, expect, it } from "vitest";
import { flattenRows } from "./flattenRows";
import { groupBranches } from "./groupBranches";
import type { BranchInfo, BranchSection } from "./types";

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

function collapseMap(entries: Record<string, boolean>) {
  const defaults = (id: string, kind: BranchSection["kind"]): boolean =>
    id in entries ? entries[id] : kind === "remote" || kind === "tags";
  return defaults;
}

describe("flattenRows", () => {
  it("emits a header row followed by depth-0 branches", () => {
    const sections = groupBranches([branch({ name: "main" }), branch({ name: "dev" })]);
    const rows = flattenRows(sections, () => false);
    expect(rows.map((r) => [r.kind, r.depth])).toEqual([
      ["section-header", 0],
      ["branch", 0],
      ["branch", 0],
    ]);
    expect(rows.every((r) => r.key.length > 0)).toBe(true);
    expect(new Set(rows.map((r) => r.key)).size).toBe(rows.length);
  });

  it("orders nested folders DFS-first with increasing depth", () => {
    const sections = groupBranches([
      branch({ name: "feat/auth/oauth" }),
      branch({ name: "feat/payments" }),
    ]);
    const rows = flattenRows(sections, () => false);
    expect(rows.map((r) => `${r.kind}:${r.depth}`)).toEqual([
      "section-header:0",
      "folder-header:0",
      "folder-header:1",
      "branch:2",
      "branch:1",
    ]);
    const folderIds = rows.filter((r) => r.kind === "folder-header");
    expect(folderIds.map((r) => ("folderId" in r ? r.folderId : ""))).toEqual([
      "local/feat",
      "local/feat/auth",
    ]);
  });

  it("collapses a mid-tree folder to just its header", () => {
    const sections = groupBranches([
      branch({ name: "feat/auth/oauth" }),
      branch({ name: "feat/payments" }),
      branch({ name: "main" }),
    ]);
    // Deep collapse hides only the auth subtree; feat keeps its own header,
    // the collapsed auth header, and its other leaf payments.
    const deep = flattenRows(sections, (id) => id === "local/feat/auth");
    expect(deep.map((r) => r.kind)).toEqual([
      "section-header",
      "folder-header",
      "folder-header",
      "branch",
      "branch",
    ]);
    const deepAuth = deep[2];
    expect(deepAuth.kind === "folder-header" && deepAuth.folderId).toBe("local/feat/auth");
    const deepLeaves = deep.flatMap((r) => (r.kind === "branch" ? [r.branch.name] : []));
    expect(deepLeaves).toEqual(["feat/payments", "main"]);
    expect(deepLeaves).not.toContain("feat/auth/oauth");
    const mid = flattenRows(sections, (id) => id === "local/feat");
    expect(mid.map((r) => r.kind)).toEqual(["section-header", "folder-header", "branch"]);
    expect(mid[1].kind === "folder-header" && mid[1].folderId).toBe("local/feat");
    expect(mid[2].kind === "branch" && mid[2].branch.name).toBe("main");
  });

  it("renders collapsed sections as their header alone", () => {
    const sections = groupBranches(
      [branch({ name: "main" })],
      [{ name: "v1.0.0", commit_id: "aaa" }]
    );
    const rows = flattenRows(sections, collapseMap({}));
    // Remote/tags default collapsed; here only the tags section exists.
    const kinds = rows.map((r) => r.kind);
    expect(kinds.filter((k) => k === "tag")).toHaveLength(0);
    const open = flattenRows(sections, () => false);
    expect(open.filter((r) => r.kind === "tag").map((r) => (r.kind === "tag" ? r.tag.name : "")))
      .toEqual(["v1.0.0"]);
    expect(open.filter((r) => r.kind === "tag").every((r) => r.depth === 0)).toBe(true);
  });

  it("keeps keys unique when leaf names repeat across folders", () => {
    const sections = groupBranches([
      branch({ name: "feat/dup" }),
      branch({ name: "bugfix/dup" }),
      branch({ name: "dup" }),
      branch({
        name: "origin/feat/dup",
        is_remote: true,
        remote_name: "origin",
      }),
    ]);
    const rows = flattenRows(sections, () => false);
    const keys = rows.map((r) => r.key);
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys.filter((k) => k.startsWith("b:local:")).sort()).toEqual([
      "b:local:bugfix/dup",
      "b:local:dup",
      "b:local:feat/dup",
    ]);
    expect(keys.filter((k) => k.startsWith("b:remote:"))).toEqual(["b:remote:origin:origin/feat/dup"]);
    expect(keys.filter((k) => k.startsWith("f:"))).toEqual(["f:local/bugfix", "f:local/feat", "f:remote:origin/feat"]);
    expect(keys.filter((k) => k.startsWith("s:"))).toEqual(["s:local", "s:remote:origin"]);
  });

  it("prefixes pinned and tag keys without a uniqueness scan", () => {
    const sections = groupBranches(
      [branch({ name: "feat/auth" }), branch({ name: "main" })],
      [{ name: "v1.0.0", commit_id: "aaa" }],
      new Set(["feat/auth"])
    );
    const rows = flattenRows(sections, () => false);
    expect(rows.map((r) => r.key)).toEqual([
      "s:pinned",
      "b:pinned:feat/auth",
      "s:local",
      "f:local/feat",
      "b:local:feat/auth",
      "b:local:main",
      "s:tags",
      "t:v1.0.0",
    ]);
  });

  it("handles filtered sections with empty branch lists", () => {
    const sections: BranchSection[] = [
      {
        id: "local",
        label: "Local",
        kind: "local",
        folders: [],
        branches: [],
        tags: [],
        branchCount: 0,
      },
    ];
    expect(flattenRows(sections, () => false).map((r) => r.kind)).toEqual(["section-header"]);
    expect(flattenRows([], () => false)).toEqual([]);
  });
});
