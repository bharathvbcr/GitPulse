import { describe, expect, it } from "vitest";
import { groupBranches } from "./groupBranches";
import { flattenRows, type FlatRow } from "./flattenRows";
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

const expand = (sections: BranchSection[]): FlatRow[] =>
  flattenRows(sections, () => false);

/** Every branch input must surface as exactly one branch row when expanded. */
function branchRows(rows: FlatRow[]): FlatRow[] {
  return rows.filter((r) => r.kind === "branch");
}

describe("groupBranches + flattenRows stress: deep-path scale", () => {
  it("groups and flattens 2k branches on a shared 40-segment spine under 2500ms", () => {
    const spine = Array.from({ length: 39 }, (_, k) => `d${String(k).padStart(2, "0")}`).join("/");
    const branches = Array.from({ length: 2_000 }, (_, i) => branch({ name: `${spine}/br-${i}` }));

    const startedAt = performance.now();
    const sections = groupBranches(branches);
    const rows = expand(sections);
    const elapsedMs = performance.now() - startedAt;

    expect(elapsedMs).toBeLessThan(2_500);
    // section header + 39 folder headers + 2000 branch rows.
    expect(rows).toHaveLength(1 + 39 + 2_000);
    const deepestFolderRows = rows.filter(
      (r) => r.kind === "folder-header" && (r as { depth: number }).depth === 38
    );
    expect(deepestFolderRows).toHaveLength(1);
    expect(branchRows(rows)).toHaveLength(2_000);
  });

  it("survives 40-deep DISJOINT paths (60 roots x 39 folders) under the same bound", () => {
    // 40 segments per name: root r{i} + 38 middles + leaf.
    const branches = Array.from({ length: 60 }, (_, r) =>
      branch({
        name: [`r${r}`, ...Array.from({ length: 38 }, (_, k) => `d${k}`), "leaf"].join("/"),
      })
    );
    const startedAt = performance.now();
    const rows = expand(groupBranches(branches));
    const elapsedMs = performance.now() - startedAt;
    expect(elapsedMs).toBeLessThan(2_500);
    expect(rows.filter((r) => r.kind === "folder-header")).toHaveLength(60 * 39);
    expect(branchRows(rows)).toHaveLength(60);
  });
});

describe("groupBranches + flattenRows stress: duplicate ref names", () => {
  it("keeps same display names on different remotes unique through the keys", () => {
    const sections = groupBranches([
      branch({ name: "origin/feat/x", is_remote: true, remote_name: "origin" }),
      branch({ name: "upstream/feat/x", is_remote: true, remote_name: "upstream" }),
    ]);
    const rows = expand(sections);
    const keys = rows.map((r) => r.key);
    expect(new Set(keys).size).toBe(keys.length);
    expect(branchRows(rows)).toHaveLength(2);
  });

  it("keeps a pinned branch distinct from its local twin", () => {
    const sections = groupBranches([branch({ name: "main", is_current: true })], [], new Set(["main"]));
    const pinned = sections.find((s) => s.kind === "pinned")!;
    const local = sections.find((s) => s.kind === "local")!;
    expect(pinned.branches.map((b) => b.name)).toEqual(["main"]);
    expect(local.branches.map((b) => b.name)).toEqual(["main"]);
    const rows = expand(sections);
    const keys = rows.map((r) => r.key);
    expect(new Set(keys).size).toBe(keys.length); // b:pinned:main vs b:local:main
  });

  it("characterizes duplicate branch names as emitting duplicate keys by design", () => {
    // flattenRows deliberately does NOT re-enforce key uniqueness (its
    // docstring documents the contract): git ref naming guarantees unique
    // full names within a section upstream, and synthetic duplicates are a
    // caller error. groupBranches tolerates them; keys collide harmlessly in
    // this characterization because production inputs cannot produce them.
    const storm = Array.from({ length: 100 }, () => branch({ name: "feat/x" }));
    const rows = expand(groupBranches(storm));
    expect(branchRows(rows)).toHaveLength(100);
    const keys = branchRows(rows).map((r) => r.key);
    expect(new Set(keys).size).toBe(1);
  });
});

describe("groupBranches + flattenRows stress: slash-hostile names", () => {
  const hostileNames = [
    "/",
    "//",
    "///",
    "a//b",
    "a/b/",
    "/lead",
    "...",
    "..",
    ".",
    ".hidden/x",
    "x/.lock",
    "name with space/sub",
    "",
  ];

  it("groups and flattens every hostile name without throwing or losing a branch", () => {
    const branches = hostileNames.map((n) => branch({ name: n }));
    const sections = groupBranches(branches);
    const rows = expand(sections);
    expect(branchRows(rows)).toHaveLength(hostileNames.length);
    for (const row of rows) {
      expect(typeof row.key).toBe("string");
      expect(row.key.length).toBeGreaterThan(0);
      if (row.kind === "branch") expect(Number.isInteger(row.depth)).toBe(true);
    }
  });

  it("lands '/lead' at root level because splitPath drops empty leading parts", () => {
    // Characterized: parts = ["lead"] → folders=[] → straight into
    // section.branches under its RAW name "/lead".
    const local = groupBranches([branch({ name: "/lead" })])[0];
    expect(local.folders).toHaveLength(0);
    expect(local.branches.map((b) => b.name)).toEqual(["/lead"]);
  });

  it("collapses interior double slashes into one folder level without crash", () => {
    // Characterized: "a//b" → folders ["a"], leaf "b"; the display name loses
    // the doubled slash but grouping stays consistent and countable.
    const local = groupBranches([branch({ name: "a//b" })])[0];
    expect(local.folders[0].label).toBe("a");
    expect(local.folders[0].branches.map((b) => b.name)).toEqual(["a//b"]);
    expect(local.branchCount).toBe(1);
  });

  it("treats dot-only segments as ordinary labels", () => {
    const local = groupBranches([
      branch({ name: "../etc" }),
      branch({ name: "./here" }),
      branch({ name: ".../ellipsis" }),
    ])[0];
    expect(local.branchCount).toBe(3);
    expect(() => expand(groupBranches(hostileNames.map((n) => branch({ name: n }))))).not.toThrow();
  });
});

describe("groupBranches + flattenRows stress: tags and section identity", () => {
  it("omits the tags section entirely when zero tags are supplied", () => {
    const sections = groupBranches([branch({ name: "main" })], []);
    expect(sections.some((s) => s.kind === "tags")).toBe(false);
  });

  it("flattens a hand-built zero-tag tags section to just its header", () => {
    const empty: BranchSection = {
      id: "tags",
      label: "Tags",
      kind: "tags",
      folders: [],
      branches: [],
      tags: [],
      branchCount: 0,
    };
    const rows = expand([empty]);
    expect(rows).toHaveLength(1);
    expect(rows[0].kind).toBe("section-header");
  });

  it("never lets real grouping emit two sections sharing an id", () => {
    // The collision below is only constructible by hand; groupBranches keys
    // remotes by remote name so ids are unique by construction. Pinned so a
    // future refactor cannot quietly break flattenRows' key uniqueness.
    const sections = groupBranches([
      branch({ name: "origin/a", is_remote: true, remote_name: "origin" }),
      branch({ name: "origin/b", is_remote: true, remote_name: "origin" }),
      branch({ name: "local-one" }),
    ]);
    const ids = sections.map((s) => s.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("characterizes hand-built same-id sections as producing duplicate header keys", () => {
    const make = (): BranchSection => ({
      id: "dup",
      label: "Dup",
      kind: "remote",
      remoteName: "dup",
      folders: [],
      branches: [branch({ name: "x", is_remote: true })],
      tags: [],
      branchCount: 1,
    });
    const rows = expand([make(), make()]);
    const headerKeys = rows.filter((r) => r.kind === "section-header").map((r) => r.key);
    expect(headerKeys).toEqual(["s:dup", "s:dup"]); // documented manual-input caveat
  });

  it("propagates a throwing collapse lookup instead of swallowing it", () => {
    // Characterization of actual behavior: flattenRows calls isCollapsed
    // per section/folder with no try/catch, so a poisoned collapse store
    // surfaces here rather than silently rendering everything expanded.
    const sections = groupBranches([branch({ name: "feat/a" }), branch({ name: "feat/b" })]);
    expect(() =>
      flattenRows(sections, () => {
        throw new Error("collapsed store poisoned");
      })
    ).toThrow("collapsed store poisoned");
  });

  it("emits folder headers even when collapsed, but hides their children", () => {
    const sections = groupBranches([
      branch({ name: "feat/a" }),
      branch({ name: "feat/sub/deep" }),
      branch({ name: "solo" }),
    ]);
    const collapsedFolders = new Set(
      sections[0].folders.map((f) => f.id)
    );
    const rows = flattenRows(sections, (id) => collapsedFolders.has(id));
    // Top-level feat folder visible, everything beneath it hidden; solo untouched.
    expect(branchRows(rows).map((r) => (r.kind === "branch" ? r.branch.name : ""))).toEqual(["solo"]);
    expect(rows.filter((r) => r.kind === "folder-header")).toHaveLength(1);
  });
});
