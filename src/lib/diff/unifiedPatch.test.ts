import { describe, expect, it } from "vitest";
import { splitUnifiedPatch } from "./unifiedPatch";

describe("splitUnifiedPatch", () => {
  it("returns empty array for blank patches", () => {
    expect(splitUnifiedPatch("")).toEqual([]);
  });

  it("parses modified, added, deleted, renamed, and binary files with line numbers", () => {
    const patch = [
      "diff --git a/src/app.ts b/src/app.ts",
      "index 1111111..2222222 100644",
      "--- a/src/app.ts",
      "+++ b/src/app.ts",
      "@@ -10,3 +10,4 @@ export function run() {",
      "   const a = 1;",
      "-  return a;",
      "+  const b = 2;",
      "+  return a + b;",
      " }",
      "diff --git a/docs/old.md b/docs/new.md",
      "similarity index 100%",
      "rename from docs/old.md",
      "rename to docs/new.md",
      "diff --git a/added.txt b/added.txt",
      "new file mode 100644",
      "--- /dev/null",
      "+++ b/added.txt",
      "@@ -0,0 +1,2 @@",
      "+hello",
      "+world",
      "\\ No newline at end of file",
      "diff --git a/removed.txt b/removed.txt",
      "deleted file mode 100644",
      "--- a/removed.txt",
      "+++ /dev/null",
      "@@ -1 +0,0 @@",
      "-gone",
      "diff --git a/logo.png b/logo.png",
      "Binary files a/logo.png and b/logo.png differ",
      "",
    ].join("\n");

    const parsed = splitUnifiedPatch(patch);
    expect(parsed).toHaveLength(5);

    expect(parsed[0]).toMatchObject({
      path: "src/app.ts",
      status: "modified",
      additions: 2,
      deletions: 1,
      binary: false,
    });
    expect(parsed[0].hunks[0].lines).toEqual([
      { kind: "ctx", oldLine: 10, newLine: 10, text: "  const a = 1;" },
      { kind: "del", oldLine: 11, newLine: null, text: "  return a;" },
      { kind: "add", oldLine: null, newLine: 11, text: "  const b = 2;" },
      { kind: "add", oldLine: null, newLine: 12, text: "  return a + b;" },
      { kind: "ctx", oldLine: 12, newLine: 13, text: "}" },
    ]);

    expect(parsed[1]).toMatchObject({
      path: "docs/new.md",
      oldPath: "docs/old.md",
      status: "renamed",
      additions: 0,
      deletions: 0,
    });

    expect(parsed[2]).toMatchObject({
      path: "added.txt",
      status: "added",
      additions: 2,
      deletions: 0,
    });
    expect(parsed[2].hunks[0].lines[2]).toEqual({
      kind: "meta",
      oldLine: null,
      newLine: null,
      text: "\\ No newline at end of file",
    });

    expect(parsed[3]).toMatchObject({
      path: "removed.txt",
      status: "deleted",
      additions: 0,
      deletions: 1,
    });

    expect(parsed[4]).toMatchObject({
      path: "logo.png",
      status: "binary",
      binary: true,
    });
  });

  it("handles filenames containing spaces and b/ substrings", () => {
    const patch = [
      'diff --git "a/dir with b/file.ts" "b/dir with b/file.ts"',
      "--- \"a/dir with b/file.ts\"",
      "+++ \"b/dir with b/file.ts\"",
      "@@ -1 +1 @@",
      "-a",
      "+b",
    ].join("\n");

    const parsed = splitUnifiedPatch(patch);
    expect(parsed).toHaveLength(1);
    expect(parsed[0].path).toBe("dir with b/file.ts");
    expect(parsed[0].additions).toBe(1);
    expect(parsed[0].deletions).toBe(1);
  });

  it("preserves stripped blank context lines, bare multi-file headers, zero-count hunks, and octal UTF-8 filenames", () => {
    const patch = [
      'diff --git "a/caf\\303\\251.ts" "b/caf\\303\\251.ts"',
      '--- "a/caf\\303\\251.ts"',
      '+++ "b/caf\\303\\251.ts"',
      "@@ -1,3 +1,3 @@",
      " a",
      "",
      "-b",
      "+c",
      "--- a/plain-one.txt",
      "+++ /dev/null",
      "@@ -1 +0,0 @@",
      "-old",
      "--- /dev/null",
      "+++ b/plain-two.txt",
      "@@ -0,0 +1,2 @@",
      "+first",
      "+second",
      "",
    ].join("\n");

    const parsed = splitUnifiedPatch(patch);
    expect(parsed).toHaveLength(3);
    expect(parsed[0].path).toBe("café.ts");
    expect(parsed[0].hunks[0].lines).toEqual([
      { kind: "ctx", oldLine: 1, newLine: 1, text: "a" },
      { kind: "ctx", oldLine: 2, newLine: 2, text: "" },
      { kind: "del", oldLine: 3, newLine: null, text: "b" },
      { kind: "add", oldLine: null, newLine: 3, text: "c" },
    ]);
    expect(parsed[1]).toMatchObject({ path: "plain-one.txt", status: "deleted", deletions: 1 });
    expect(parsed[2]).toMatchObject({ path: "plain-two.txt", status: "added", additions: 2 });
  });
});
