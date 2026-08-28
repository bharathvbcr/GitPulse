import { describe, expect, it } from "vitest";
import {
  ancestorsOf,
  buildFileTree,
  filterPathsByQuery,
  flattenFileTree,
  isValidRelativePath,
  joinWorktreePath,
  type FileRow,
} from "./fileTree";

const rowsOf = (paths: string[], collapsed: (p: string) => boolean = () => false): FileRow[] =>
  flattenFileTree(buildFileTree(paths), collapsed);

describe("buildFileTree", () => {
  it("nests segments and separates root files", () => {
    const tree = buildFileTree(["src/lib/a.ts", "src/b.ts", "README.md"]);
    expect(tree.files).toEqual(["README.md"]);
    expect(tree.dirs).toHaveLength(1);
    const src = tree.dirs[0];
    expect(src).toMatchObject({ name: "src", path: "src" });
    expect(src.files).toEqual(["src/b.ts"]);
    expect(src.dirs[0]).toMatchObject({ name: "lib", path: "src/lib" });
    expect(src.dirs[0].files).toEqual(["src/lib/a.ts"]);
  });

  it("sorts dirs before same-named files and uses one collator per level", () => {
    // "report" the dir vs "report" the file cannot coexist in git, but the
    // merge ordering must still be deterministic when names tie.
    const rows = rowsOf(["docs/guide.md", "docs/api.md", "a.txt"]);
    // One shared collator per level: root children sort by name regardless of
    // kind, so the file "a.txt" precedes the dir "docs".
    expect(rows.map((r) => r.path)).toEqual(["a.txt", "docs", "docs/api.md", "docs/guide.md"]);
  });

  it("rejects traversal escapes instead of coercing them into rows", () => {
    const hostile = [
      "/absolute/path.txt",
      "..",
      "../escape.txt",
      "a/./b.txt",
      "a/../b.txt",
      "double//slash.txt",
      "trailing/",
      "",
      "   ",
      "\\\\windows\\\\path",
      // Windows drive letters are absolute in spirit even after slash
      // normalization; accepting one fabricates a phantom "C:" dir.
      "C:/evil/payload.txt",
      "c:/relative-looking.txt",
      42 as unknown as string,
      null as unknown as string,
    ];
    const tree = buildFileTree(hostile);
    expect(tree.dirs).toHaveLength(0);
    expect(tree.files).toHaveLength(0);
  });

  it("never fabricates ancestors for absolute or drive-qualified paths", () => {
    expect(ancestorsOf("C:/x/y.txt")).toEqual([]);
    expect(ancestorsOf("/etc/passwd")).toEqual([]);
    expect(ancestorsOf("a/b/c.txt")).toEqual(["a", "a/b"]);
  });

  it("keeps well-formed siblings of rejected entries", () => {
    const tree = buildFileTree(["../evil", "keep/me.txt", "./dot", "ok.txt"]);
    expect(tree.files).toEqual(["ok.txt"]);
    expect(tree.dirs.map((d) => d.path)).toEqual(["keep"]);
    expect(tree.dirs[0].files).toEqual(["keep/me.txt"]);
  });

  it("backslashes normalize to forward slashes for nesting", () => {
    const tree = buildFileTree(["src\\lib\\x.rs"]);
    expect(tree.dirs[0].path).toBe("src");
    expect(tree.dirs[0].dirs[0].files).toEqual(["src/lib/x.rs"]);
  });

  it("lets a dir slot win over a later file claim at the same name", () => {
    const tree = buildFileTree(["weird/file.txt", "weird"]);
    const weird = tree.dirs.find((d) => d.name === "weird");
    expect(weird?.files).toEqual(["weird/file.txt"]);
    expect(tree.files).not.toContain("weird");
  });

  it("is deterministic across reruns (identical JSON)", () => {
    const paths = ["z/y.ts", "a/b/c.ts", "a/d.txt"];
    expect(JSON.stringify(buildFileTree(paths))).toBe(JSON.stringify(buildFileTree([...paths])));
  });
});

describe("flattenFileTree", () => {
  it("assigns depth by nesting and stable keyed identities", () => {
    const rows = rowsOf(["src/lib/deep.ts", "top.ts"]);
    expect(rows.map((r) => [r.kind, r.depth])).toEqual([
      ["dir", 0],
      ["dir", 1],
      ["file", 2],
      ["file", 0],
    ]);
    const keys = new Set(rows.map((r) => r.key));
    expect(keys.size).toBe(rows.length);
  });

  it("collapsing a dir hides its entire subtree but keeps the dir row", () => {
    const rows = rowsOf(["src/lib/a.ts", "src/lib/sub/b.ts", "src/main.rs"], (p) => p === "src/lib");
    // The collapsed dir row stays in place among its sorted siblings; only
    // the subtree beneath it disappears.
    expect(rows.map((r) => r.path)).toEqual(["src", "src/lib", "src/main.rs"]);
  });

  it("interleaves dirs and files alphabetically at each level", () => {
    const rows = rowsOf(["m/alpha/x.txt", "beta.txt", "m/zeta.txt"]);
    // Root level: beta.txt < m. Inside m: alpha(dir) < zeta.txt(file).
    expect(rows.map((r) => r.name)).toEqual(["beta.txt", "m", "alpha", "x.txt", "zeta.txt"]);
  });
});

describe("filterPathsByQuery", () => {
  const paths = ["src/lib/main.ts", "src/App.svelte", "README.md"];

  it("keeps everything on empty or whitespace queries", () => {
    expect(filterPathsByQuery(paths, "")).toEqual(paths);
    expect(filterPathsByQuery(paths, "  ")).toEqual(paths);
  });

  it("matches basename OR full path, case-insensitively", () => {
    expect(filterPathsByQuery(paths, "MAIN")).toEqual(["src/lib/main.ts"]);
    expect(filterPathsByQuery(paths, "src/app")).toEqual(["src/App.svelte"]);
    expect(filterPathsByQuery(paths, ".md")).toEqual(["README.md"]);
  });

  it("returns a new array even when nothing is filtered out", () => {
    const out = filterPathsByQuery(paths, "");
    expect(out).not.toBe(paths);
    expect(out).toEqual(paths);
  });
});

describe("ancestorsOf", () => {
  it("returns nearest-last ancestor dirs", () => {
    expect(ancestorsOf("a/b/c.txt")).toEqual(["a", "a/b"]);
    expect(ancestorsOf("root.txt")).toEqual([]);
    // A leading slash is not a repo-relative ancestor boundary.
    expect(ancestorsOf("/etc/passwd")).toEqual([]);
  });
});

describe("joinWorktreePath", () => {
  it("joins a repo root to a validated relative path", () => {
    expect(joinWorktreePath("/Users/acme/repo", "src/a.ts")).toBe("/Users/acme/repo/src/a.ts");
    expect(joinWorktreePath("/Users/acme/repo/", "README.md")).toBe("/Users/acme/repo/README.md");
  });

  it("refuses traversal, absolute, and empty inputs", () => {
    expect(joinWorktreePath("/repo", "../escape.ts")).toBeNull();
    expect(joinWorktreePath("/repo", "/etc/passwd")).toBeNull();
    expect(joinWorktreePath("", "a.ts")).toBeNull();
    expect(joinWorktreePath("/repo", "")).toBeNull();
    expect(isValidRelativePath("src/a.ts")).toBe(true);
    expect(isValidRelativePath("../x")).toBe(false);
  });
});
