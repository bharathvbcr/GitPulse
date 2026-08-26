import { describe, expect, it } from "vitest";
import { ancestorsOf, buildFileTree, filterPathsByQuery, flattenFileTree } from "./fileTree";
import type { FileTree, FileTreeDir } from "./fileTree";

/** Recursive dir/file totals used by expansion-count assertions. */
function countTree(tree: FileTree): { dirs: number; files: number } {
  let dirs = 0;
  let files = tree.files.length;
  const walk = (dir: FileTreeDir): void => {
    dirs += 1;
    files += dir.files.length;
    dir.dirs.forEach(walk);
  };
  tree.dirs.forEach(walk);
  return { dirs, files };
}

function findDir(tree: FileTree, path: string): FileTreeDir | undefined {
  const stack = [...tree.dirs];
  while (stack.length > 0) {
    const dir = stack.pop()!;
    if (dir.path === path) return dir;
    stack.push(...dir.dirs);
  }
  return undefined;
}

describe("buildFileTree", () => {
  it("puts flat root-level files in FileTree.files sorted", () => {
    const tree = buildFileTree(["z.ts", "a.md", "m.rs"]);
    expect(tree.dirs).toEqual([]);
    expect(tree.files).toEqual(["a.md", "m.rs", "z.ts"]);
  });

  it("creates intermediate dirs for nested paths", () => {
    const tree = buildFileTree(["src/lib/a.ts"]);
    expect(tree.files).toEqual([]);
    expect(tree.dirs).toHaveLength(1);
    const src = tree.dirs[0];
    expect(src.path).toBe("src");
    expect(src.name).toBe("src");
    expect(src.files).toEqual([]);
    expect(src.dirs).toHaveLength(1);
    expect(src.dirs[0].path).toBe("src/lib");
    expect(src.dirs[0].name).toBe("lib");
    expect(src.dirs[0].files).toEqual(["src/lib/a.ts"]);
  });

  it("shares one parent object across sibling-path traversals", () => {
    const tree = buildFileTree(["src/a.ts", "src/lib/b.ts", "src/lib/deep/c.ts"]);
    const srcViaSiblingA = findDir(tree, "src");
    expect(srcViaSiblingA).toBe(tree.dirs[0]);
    // The lib folder reached through the first insertion IS the one holding
    // b.ts, and deep/ hangs off that same object.
    const src = tree.dirs[0];
    const lib = src.dirs.find((d) => d.name === "lib")!;
    expect(lib.files).toContain("src/lib/b.ts");
    expect(lib.dirs.map((d) => d.path)).toEqual(["src/lib/deep"]);
  });

  it("dedupes identical normalized entries silently", () => {
    // "src/a.ts" and "src/./a.ts" normalize to the same joined path.
    const tree = buildFileTree(["README.md", "README.md", "src/a.ts", "src/a.ts", "src/./a.ts"]);
    const counts = countTree(tree);
    expect(counts).toEqual({ dirs: 1, files: 2 });
    expect(tree.files).toEqual(["README.md"]);
  });

  it("sorts dirs and files separately in collator order (pinned)", () => {
    // Pinned against Node's default Intl.Collator on this platform:
    // compare("A","b") < 0; compare("a b.txt","a.md") < 0;
    // compare("a.md","A.ts") < 0.
    const tree = buildFileTree(["b/file", "A/x", "a.md", "A.ts", "a b.txt"]);
    expect(tree.dirs.map((d) => d.name)).toEqual(["A", "b"]);
    expect(tree.files).toEqual(["a b.txt", "a.md", "A.ts"]);
    const a = tree.dirs[0];
    expect(a.files).toEqual(["A/x"]);
  });

  it("sorts nested dirs at every level", () => {
    const tree = buildFileTree(["src/zeta/f.ts", "src/alpha/e.ts"]);
    const src = tree.dirs[0];
    expect(src.dirs.map((d) => d.name)).toEqual(["alpha", "zeta"]);
  });

  it("treats a trailing slash as a directory marker, never a phantom leaf", () => {
    const tree = buildFileTree(["vendor/libfoo/", "vendor/libfoo/placeholder.txt"]);
    const vendor = findDir(tree, "vendor");
    expect(vendor!.path).toBe("vendor");
    // A trailing slash marks a DIRECTORY entry (submodule edge): it
    // materializes the folder chain but must not land a same-named leaf —
    // a phantom file row would produce a dead blame link when clicked.
    expect(vendor!.files).toEqual([]);
    expect(vendor!.dirs.map((d) => d.name)).toEqual(["libfoo"]);
    expect(vendor!.dirs[0].files).toEqual(["vendor/libfoo/placeholder.txt"]);
  });

  it("drops '.' segments wherever they appear", () => {
    const tree = buildFileTree(["./src/a.ts", "src/./nested/b.ts", "."]);
    const counts = countTree(tree);
    // "." alone normalizes to nothing and is skipped entirely.
    expect(counts).toEqual({ dirs: 2, files: 2 });
    expect(findDir(tree, "src")!.files).toEqual(["src/a.ts"]);
    expect(findDir(tree, "src/nested")!.files).toEqual(["src/nested/b.ts"]);
  });

  it("skips '..' entries entirely while keeping siblings present", () => {
    const tree = buildFileTree(["../escape", "src/../ok.ts", "safe.ts"]);
    const counts = countTree(tree);
    expect(counts).toEqual({ dirs: 0, files: 1 });
    expect(tree.files).toEqual(["safe.ts"]);
    expect(findDir(tree, "src")).toBeUndefined();
  });

  it("skips absolute paths", () => {
    const tree = buildFileTree(["/etc/passwd", "ok.ts"]);
    expect(tree.files).toEqual(["ok.ts"]);
    expect(countTree(tree).dirs).toBe(0);
  });

  it("skips empty and whitespace-only entries", () => {
    const tree = buildFileTree(["", "   ", "\t", "real.ts"]);
    expect(tree.files).toEqual(["real.ts"]);
  });

  it("normalizes backslashes to forward slashes", () => {
    const tree = buildFileTree(["src\\lib\\a.ts"]);
    expect(tree.dirs).toHaveLength(1);
    expect(tree.dirs[0].path).toBe("src");
    expect(findDir(tree, "src/lib")!.files).toEqual(["src/lib/a.ts"]);
  });

  it("returns an empty tree for empty input", () => {
    expect(buildFileTree([])).toEqual({ dirs: [], files: [] });
  });
});

describe("flattenFileTree", () => {
  it("emits exactly dirs+files rows when fully expanded", () => {
    const tree = buildFileTree([
      "README.md",
      "src/main.ts",
      "src/lib/a.ts",
      "src/lib/b.ts",
      "docs/guide.md",
    ]);
    const totals = countTree(tree);
    const rows = flattenFileTree(tree, () => false);
    expect(rows).toHaveLength(totals.dirs + totals.files);
  });

  it("assigns correct depths across a 3-deep chain", () => {
    const tree = buildFileTree(["a/b/c/f.txt", "root.ts"]);
    const rows = flattenFileTree(tree, () => false);
    expect(rows.map((r) => `${r.kind}:${r.depth}`)).toEqual([
      "dir:0",
      "dir:1",
      "dir:2",
      "file:3",
      "file:0",
    ]);
    expect(rows[0].name).toBe("a");
    expect(rows[1].name).toBe("b");
    expect(rows[2].name).toBe("c");
    expect(rows[3].name).toBe("f.txt");
    expect(rows[4].name).toBe("root.ts");
  });

  it("collapses a mid-tree folder to just its header", () => {
    const tree = buildFileTree([
      "src/auth/oauth.ts",
      "src/payments.ts",
      "main.ts",
    ]);
    const collapsed = flattenFileTree(tree, (p) => p === "src/auth");
    expect(collapsed.map((r) => r.kind)).toEqual(["dir", "dir", "file", "file"]);
    expect(collapsed[1].kind === "dir" && collapsed[1].path).toBe("src/auth");
    const names = collapsed.flatMap((r) => (r.kind === "file" ? [r.path] : []));
    expect(names).toEqual(["src/payments.ts", "main.ts"]);
    expect(names).not.toContain("src/auth/oauth.ts");

    // Collapsing the whole top dir hides everything beneath it.
    const topCollapsed = flattenFileTree(tree, (p) => p === "src");
    expect(topCollapsed.map((r) => r.kind)).toEqual(["dir", "file"]);
    expect(topCollapsed[0].kind === "dir" && topCollapsed[0].path).toBe("src");
  });

  it("keeps keys unique across the whole row list via prefixed paths", () => {
    const tree = buildFileTree([
      "README.md",
      "src/readme.md",
      "src/a.ts",
      "docs/src/b.ts",
    ]);
    const rows = flattenFileTree(tree, () => false);
    const keys = rows.map((r) => r.key);
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys.filter((k) => k.startsWith("f:"))).toContain("f:README.md");
    expect(keys.filter((k) => k.startsWith("f:"))).toContain("f:src/readme.md");
  });

  it("is deterministic: two runs over the same inputs deep-equal", () => {
    const tree = buildFileTree(["z/x/y.ts", "a.ts", "m/n.ts", "z/q.ts"]);
    const once = flattenFileTree(tree, () => false);
    const twice = flattenFileTree(tree, () => false);
    expect(once).toEqual(twice);
  });

  it("orders subdirs before files inside each dir, root files last", () => {
    const tree = buildFileTree(["src/zfile.ts", "src/adir/k.ts", "aaa.ts"]);
    const rows = flattenFileTree(tree, () => false);
    expect(rows.map((r) => `${r.kind}:${r.name}`)).toEqual([
      "dir:src",
      "dir:adir",
      "file:k.ts",
      "file:zfile.ts",
      "file:aaa.ts",
    ]);
  });

  it("flattens an empty tree to no rows", () => {
    expect(flattenFileTree({ dirs: [], files: [] }, () => false)).toEqual([]);
  });
});

describe("filterPathsByQuery", () => {
  const paths = ["src/lib/file.ts", "src/main.ts", "docs/readme.md", "README.md"];

  it("returns all paths for an empty query as a fresh copy in input order", () => {
    const result = filterPathsByQuery(paths, "");
    expect(result).toEqual(paths);
    expect(result).not.toBe(paths);
  });

  it("treats a whitespace-only query as empty", () => {
    expect(filterPathsByQuery(paths, "   ")).toEqual(paths);
  });

  it("matches substrings spanning directory parts", () => {
    expect(filterPathsByQuery(paths, "lib/fil")).toEqual(["src/lib/file.ts"]);
  });

  it("matches case-insensitively against the full path", () => {
    expect(filterPathsByQuery(paths, "SRC/MAIN")).toEqual(["src/main.ts"]);
    expect(filterPathsByQuery(paths, "Docs")).toEqual(["docs/readme.md"]);
  });

  it("pins plain substring semantics including partial basename hits", () => {
    // "ead" is a mid-word substring of README — substring means substring.
    expect(filterPathsByQuery(paths, "ead")).toEqual(["docs/readme.md", "README.md"]);
    expect(filterPathsByQuery(paths, "zzz")).toEqual([]);
  });

  it("preserves input order of survivors without re-sorting", () => {
    const shuffled = ["zeta/x.ts", "Alpha/y.ts", "zeta/a.ts"];
    expect(filterPathsByQuery(shuffled, "zeta")).toEqual(["zeta/x.ts", "zeta/a.ts"]);
  });
});

describe("ancestorsOf", () => {
  it("returns cumulative prefixes nearest-last for nested paths", () => {
    expect(ancestorsOf("src/lib/a.ts")).toEqual(["src", "src/lib"]);
    expect(ancestorsOf("a/b/c/d")).toEqual(["a", "a/b", "a/b/c"]);
  });

  it("returns [] for root-level files", () => {
    expect(ancestorsOf("README.md")).toEqual([]);
  });

  it("returns [] for empty-string input", () => {
    expect(ancestorsOf("")).toEqual([]);
  });

  it("tolerates trailing slashes by dropping empty segments", () => {
    expect(ancestorsOf("src/lib/")).toEqual(["src"]);
    expect(ancestorsOf("src/")).toEqual([]);
  });
});
