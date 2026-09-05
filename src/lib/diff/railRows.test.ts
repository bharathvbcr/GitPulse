import { describe, expect, it } from "vitest";
import {
  activeRowIndex,
  buildRailRows,
  disambiguatePaths,
  entryMatchesQuery,
  filterNote,
  type RailFileRow,
} from "./railRows";
import type { RailEntry } from "./fileRail";

const entry = (path: string, over: Partial<RailEntry> = {}): RailEntry => ({
  path,
  statusCode: "M",
  additions: 1,
  deletions: 0,
  isStaged: false,
  ...over,
});

const files = (rows: ReturnType<typeof buildRailRows>["rows"]) =>
  rows.filter((row): row is RailFileRow => row.kind === "file");

describe("disambiguatePaths", () => {
  it("adds nothing when basenames are already unique", () => {
    const map = disambiguatePaths(["src/a.ts", "docs/b.md"]);
    expect(map.get("src/a.ts")).toBe("");
    expect(map.get("docs/b.md")).toBe("");
  });

  it("adds exactly one parent when that settles it", () => {
    // The regression: this repository's own head commit listed `mod.rs` eight
    // times and `plugin.json` three times, in a 224px column.
    const map = disambiguatePaths([
      "src/analyzer/mod.rs",
      "src/codeintel/mod.rs",
      "src/desktop/mod.rs",
    ]);
    expect(map.get("src/analyzer/mod.rs")).toBe("analyzer");
    expect(map.get("src/codeintel/mod.rs")).toBe("codeintel");
    expect(map.get("src/desktop/mod.rs")).toBe("desktop");
  });

  it("goes deeper only for the paths that need it", () => {
    const map = disambiguatePaths([
      "a/deep/thing/mod.rs",
      "b/deep/thing/mod.rs",
      "solo/other.rs",
    ]);
    expect(map.get("a/deep/thing/mod.rs")).toBe("a/deep/thing");
    expect(map.get("b/deep/thing/mod.rs")).toBe("b/deep/thing");
    // Unique from the first round: it never grew a prefix it did not need.
    expect(map.get("solo/other.rs")).toBe("");
  });

  it("stops at the full path when two paths differ only at the root", () => {
    const map = disambiguatePaths(["x/same/name.ts", "y/same/name.ts"]);
    expect(map.get("x/same/name.ts")).toBe("x/same");
    expect(map.get("y/same/name.ts")).toBe("y/same");
  });

  it("does not invent a difference between two genuinely identical paths", () => {
    // The staged and unstaged sides of one file share a path; they are told
    // apart by the staged badge, not by a fabricated label.
    const map = disambiguatePaths(["src/a.ts", "src/a.ts"]);
    expect(map.size).toBe(1);
    expect(map.get("src/a.ts")).toBe("");
  });

  it("terminates when one path is a suffix of another", () => {
    const map = disambiguatePaths(["mod.rs", "src/mod.rs", "a/src/mod.rs"]);
    expect(map.get("mod.rs")).toBe("");
    expect(map.get("src/mod.rs")).toBe("src");
    expect(map.get("a/src/mod.rs")).toBe("a/src");
  });

  it("handles a root-level file with no directory at all", () => {
    expect(disambiguatePaths(["README.md"]).get("README.md")).toBe("");
  });

  it("returns an empty map for no paths", () => {
    expect(disambiguatePaths([]).size).toBe(0);
  });

  it("survives a thousand same-named files without hanging", () => {
    const paths = Array.from({ length: 1_000 }, (_, i) => `pkg/p${i}/index.ts`);
    const started = Date.now();
    const map = disambiguatePaths(paths);
    expect(Date.now() - started).toBeLessThan(500);
    expect(map.get("pkg/p0/index.ts")).toBe("p0");
    expect(map.get("pkg/p999/index.ts")).toBe("p999");
  });
});

describe("buildRailRows list mode", () => {
  it("keeps git's order and labels each row distinguishably", () => {
    const result = buildRailRows({
      entries: [entry("src/analyzer/mod.rs"), entry("src/codeintel/mod.rs")],
      mode: "list",
      query: "",
    });
    expect(files(result.rows).map((row) => [row.dir, row.name])).toEqual([
      ["analyzer", "mod.rs"],
      ["codeintel", "mod.rs"],
    ]);
    expect(result.matched).toBe(2);
    expect(result.total).toBe(2);
  });

  it("keeps the rename arrow in the name and the full move in the tooltip", () => {
    const rows = files(
      buildRailRows({
        entries: [entry("new/name.ts", { oldPath: "old/name.ts" })],
        mode: "list",
        query: "",
      }).rows,
    );
    expect(rows[0].name).toBe("name.ts → name.ts");
    expect(rows[0].title).toBe("old/name.ts → new/name.ts");
  });

  it("gives the staged and unstaged sides of one path distinct keys", () => {
    const rows = files(
      buildRailRows({
        entries: [entry("a.ts", { isStaged: true }), entry("a.ts")],
        mode: "list",
        query: "",
      }).rows,
    );
    expect(new Set(rows.map((row) => row.key)).size).toBe(2);
  });

  it("filters on the full path, not only the basename", () => {
    const result = buildRailRows({
      entries: [entry("src/analyzer/mod.rs"), entry("docs/guide.md")],
      mode: "list",
      query: "analyz",
    });
    expect(files(result.rows).map((row) => row.entry.path)).toEqual(["src/analyzer/mod.rs"]);
    expect(result.matched).toBe(1);
    expect(result.total).toBe(2);
  });

  it("finds a renamed file by where it used to live", () => {
    const result = buildRailRows({
      entries: [entry("new/name.ts", { oldPath: "legacy/name.ts" })],
      mode: "list",
      query: "legacy",
    });
    expect(result.matched).toBe(1);
  });

  it("reports zero matches rather than silently showing everything", () => {
    const result = buildRailRows({
      entries: [entry("a.ts")],
      mode: "list",
      query: "zzz",
    });
    expect(result.rows).toEqual([]);
    expect(result.matched).toBe(0);
    expect(result.total).toBe(1);
  });

  it("treats a whitespace-only query as no query", () => {
    expect(buildRailRows({ entries: [entry("a.ts")], mode: "list", query: "   " }).matched).toBe(1);
  });
});

describe("buildRailRows tree mode", () => {
  it("groups files under their directories", () => {
    // Ordering is the shared explorer's: dirs and files interleave
    // alphabetically at each level, so the two lists shape a repository the
    // same way instead of drifting apart.
    const result = buildRailRows({
      entries: [entry("src/a.ts"), entry("src/nested/b.ts"), entry("README.md")],
      mode: "tree",
      query: "",
    });
    expect(result.rows.map((row) => [row.kind, row.kind === "dir" ? row.path : row.entry.path, row.depth]))
      .toEqual([
        ["file", "README.md", 0],
        ["dir", "src", 0],
        ["file", "src/a.ts", 1],
        ["dir", "src/nested", 1],
        ["file", "src/nested/b.ts", 2],
      ]);
  });

  it("sums churn onto each ancestor directory", () => {
    const result = buildRailRows({
      entries: [
        entry("src/deep/a.ts", { additions: 5, deletions: 1 }),
        entry("src/b.ts", { additions: 2, deletions: 3 }),
      ],
      mode: "tree",
      query: "",
    });
    const src = result.rows.find((row) => row.kind === "dir" && row.path === "src");
    expect(src).toMatchObject({ fileCount: 2, additions: 7, deletions: 4 });
    const deep = result.rows.find((row) => row.kind === "dir" && row.path === "src/deep");
    expect(deep).toMatchObject({ fileCount: 1, additions: 5, deletions: 1 });
  });

  it("hides a collapsed directory's whole subtree", () => {
    const result = buildRailRows({
      entries: [entry("src/a.ts"), entry("src/nested/b.ts"), entry("README.md")],
      mode: "tree",
      query: "",
      isCollapsed: (dir) => dir === "src",
    });
    expect(result.rows.map((row) => (row.kind === "dir" ? row.path : row.entry.path))).toEqual([
      "README.md",
      "src",
    ]);
  });

  it("emits both sides of a staged/unstaged path under one directory", () => {
    const result = buildRailRows({
      entries: [entry("src/a.ts", { isStaged: true }), entry("src/a.ts")],
      mode: "tree",
      query: "",
    });
    expect(files(result.rows)).toHaveLength(2);
    expect(new Set(files(result.rows).map((row) => row.key)).size).toBe(2);
  });

  it("folds only what survived the filter", () => {
    const result = buildRailRows({
      entries: [entry("src/a.ts"), entry("docs/b.md")],
      mode: "tree",
      query: "docs",
    });
    expect(result.rows.map((row) => (row.kind === "dir" ? row.path : row.entry.path))).toEqual([
      "docs",
      "docs/b.md",
    ]);
  });

  it("keeps a path the shared tree builder rejects rather than dropping it", () => {
    // `buildFileTree` refuses absolute and `..` paths. A rail that silently
    // showed fewer files than the list mode does would be reporting a
    // shorter commit than the one on screen.
    const result = buildRailRows({
      entries: [entry("src/ok.ts"), entry("../escape.ts"), entry("/abs.ts")],
      mode: "tree",
      query: "",
    });
    const paths = files(result.rows).map((row) => row.entry.path);
    expect(paths).toContain("../escape.ts");
    expect(paths).toContain("/abs.ts");
    expect(paths).toHaveLength(3);
    expect(result.matched).toBe(3);
  });

  it("adds no directory chrome in list mode", () => {
    const result = buildRailRows({
      entries: [entry("src/nested/b.ts")],
      mode: "list",
      query: "",
    });
    expect(result.rows.every((row) => row.kind === "file")).toBe(true);
  });
});

describe("activeRowIndex", () => {
  const rows = buildRailRows({
    entries: [entry("a.ts", { isStaged: true }), entry("a.ts"), entry("b.ts")],
    mode: "list",
    query: "",
  }).rows;

  it("distinguishes the staged side when the rail has both", () => {
    expect(activeRowIndex(rows, "a.ts", true, true)).toBe(0);
    expect(activeRowIndex(rows, "a.ts", false, true)).toBe(1);
  });

  it("matches on path alone for a commit rail, which has no staged side", () => {
    expect(activeRowIndex(rows, "a.ts", true, false)).toBe(0);
  });

  it("is -1 for a file filtered out of the list, not 0", () => {
    expect(activeRowIndex(rows, "gone.ts", false, true)).toBe(-1);
    expect(activeRowIndex(rows, null, false, true)).toBe(-1);
  });

  it("skips directory rows when searching a tree", () => {
    const tree = buildRailRows({
      entries: [entry("src/a.ts")],
      mode: "tree",
      query: "",
    }).rows;
    expect(activeRowIndex(tree, "src/a.ts", false, true)).toBe(1);
  });
});

describe("entryMatchesQuery / filterNote", () => {
  it("matches case-insensitively", () => {
    expect(entryMatchesQuery(entry("SRC/App.ts"), "src/app")).toBe(true);
  });

  it("matches everything on an empty needle", () => {
    expect(entryMatchesQuery(entry("a.ts"), "")).toBe(true);
  });

  it("says nothing while no filter is on", () => {
    expect(filterNote({ rows: [], matched: 0, total: 5 }, "")).toBe("");
  });

  it("counts matches against the whole list", () => {
    expect(filterNote({ rows: [], matched: 3, total: 200 }, "mod")).toBe("3 of 200 files");
  });

  it("names the query it found nothing for", () => {
    expect(filterNote({ rows: [], matched: 0, total: 200 }, " zzz ")).toBe("no files match “zzz”");
  });
});

describe("row names", () => {
  const nameOf = (over: Partial<RailEntry>) =>
    (buildRailRows({ entries: [entry("x", over)], mode: "list", query: "" })
      .rows[0] as RailFileRow).name;

  it("names a file by its basename", () => {
    expect(nameOf({ path: "src/lib/a.ts" })).toBe("a.ts");
  });

  it("shows a rename as old → new", () => {
    expect(nameOf({ path: "src/b.ts", oldPath: "src/a.ts" })).toBe("a.ts → b.ts");
  });

  it("does not render an arrow when a rename kept the name", () => {
    expect(nameOf({ path: "src/a.ts", oldPath: "src/a.ts" })).toBe("a.ts");
  });

  it("survives a path with no directory and a trailing slash", () => {
    // git emits directory-shaped paths for submodule and mode entries; a
    // trailing slash must not produce a nameless row.
    expect(nameOf({ path: "a.ts" })).toBe("a.ts");
    expect(nameOf({ path: "src/dir/" })).toBe("dir");
  });
});
