import { describe, expect, it } from "vitest";
import {
  annotateRange,
  annotateUnifiedDiff,
  computeWordDiff,
  emptyDiffCopy,
  filterFilePatch,
  isImagePath,
  parseUnifiedDiff,
} from "./wordDiff";

describe("computeWordDiff", () => {
  it("highlights a single replaced number", () => {
    const diff = computeWordDiff("let count = 42;", "let count = 100;");
    expect(diff.original_segments).toEqual([
      { kind: "Equal", text: "let count = " },
      { kind: "Removed", text: "42" },
      { kind: "Equal", text: ";" },
    ]);
    expect(diff.modified_segments).toEqual([
      { kind: "Equal", text: "let count = " },
      { kind: "Added", text: "100" },
      { kind: "Equal", text: ";" },
    ]);
  });

  it("short-circuits huge minified lines", () => {
    const diff = computeWordDiff("a".repeat(60_000), "b".repeat(60_000));
    expect(diff.original_segments).toHaveLength(1);
    expect(diff.original_segments[0].kind).toBe("Removed");
    expect(diff.modified_segments[0].kind).toBe("Added");
  });
});

describe("annotateUnifiedDiff", () => {
  it("pairs adjacent delete/add lines with word segments", () => {
    const lines = annotateUnifiedDiff("@@ -1 +1 @@\n-foo bar\n+foo baz\n");
    expect(lines[1].type).toBe("del");
    expect(lines[1].segments?.some((s) => s.kind === "Removed" && s.text === "bar")).toBe(true);
    expect(lines[2].segments?.some((s) => s.kind === "Added" && s.text === "baz")).toBe(true);
  });
});

describe("isImagePath", () => {
  it("detects common image extensions", () => {
    expect(isImagePath("logo.png")).toBe(true);
    expect(isImagePath("src/main.rs")).toBe(false);
  });
});

describe("parseUnifiedDiff", () => {
  const sample = [
    "diff --git a/src/a.rs b/src/a.rs",
    "--- a/src/a.rs",
    "+++ b/src/a.rs",
    "@@ -3,4 +3,5 @@ fn main() {",
    " ctx keeps both",
    "-old line one",
    "+new line one",
    " more context",
    "\\ No newline at end of file",
  ].join("\n");

  it("tracks real line numbers from hunk headers", () => {
    const lines = parseUnifiedDiff(sample);
    const ctx = lines.find((l) => l.content === " ctx keeps both");
    expect(ctx?.oldNo).toBe(3);
    expect(ctx?.newNo).toBe(3);
    const del = lines.find((l) => l.type === "del");
    expect(del?.oldNo).toBe(4);
    expect(del?.newNo).toBeUndefined();
    const add = lines.find((l) => l.type === "add");
    expect(add?.newNo).toBe(4);
    const after = lines.find((l) => l.content === " more context");
    // del consumed old line 4; add consumed new line 4, so the next shared
    // row is 5 in both files.
    expect(after?.oldNo).toBe(5);
    expect(after?.newNo).toBe(5);
  });

  it("classifies no-newline markers as headers, not content", () => {
    const lines = parseUnifiedDiff(sample);
    const marker = lines.find((l) => l.content.startsWith("\\"));
    expect(marker?.type).toBe("hdr");
  });

  it("tolerates missing hunk headers by omitting numbers instead of lying", () => {
    const lines = parseUnifiedDiff("-x\n+y\n");
    expect(lines[0].oldNo).toBeUndefined();
    expect(lines[1].newNo).toBeUndefined();
  });
});

describe("annotateRange", () => {
  function pairDiff(count: number): string {
    const parts: string[] = ["@@ -1 +1 @@"];
    for (let i = 0; i < count; i++) {
      parts.push(`-old ${i}`, `+new ${i}`);
    }
    return parts.join("\n");
  }

  it("annotates only pairs inside the requested window", () => {
    const parsed = parseUnifiedDiff(pairDiff(50));
    // Window over rows 10..14: those pairs get segments, nothing else does.
    annotateRange(parsed, 11, 13);
    const annotated = parsed.filter((l) => l.segments !== undefined).length;
    expect(annotated).toBeGreaterThan(0);
    expect(annotated).toBeLessThanOrEqual(2);
  });

  it("skips pairs already annotated (memoized segments stay put)", () => {
    const parsed = parseUnifiedDiff("@@ -1 +1 @@\n-foo bar\n+foo baz\n");
    annotateRange(parsed, 0, parsed.length);
    const withSegments = parsed.map((l) => l.segments);
    // A second pass must not clear or recompute them.
    annotateRange(parsed, 0, parsed.length);
    expect(parsed.map((l) => l.segments)).toEqual(withSegments);
  });

  it("pairs a multi-line replacement block, not only the first adjacent pair", () => {
    const parsed = parseUnifiedDiff(
      ["@@ -1,3 +1,3 @@", "-alpha one", "-beta two", "-gamma three", "+ALPHA one", "+BETA two", "+GAMMA three"].join(
        "\n"
      )
    );
    annotateRange(parsed, 0, parsed.length);
    const dels = parsed.filter((l) => l.type === "del");
    const adds = parsed.filter((l) => l.type === "add");
    expect(dels).toHaveLength(3);
    expect(adds).toHaveLength(3);
    expect(dels.every((l) => l.segments && l.segments.length > 0)).toBe(true);
    expect(adds.every((l) => l.segments && l.segments.length > 0)).toBe(true);
    expect(dels[1].segments?.some((s) => s.kind === "Removed" && s.text.includes("beta"))).toBe(true);
    expect(adds[1].segments?.some((s) => s.kind === "Added" && s.text.includes("BETA"))).toBe(true);
  });

  it("returns an empty slice for out-of-bounds ranges", () => {
    const parsed = parseUnifiedDiff("+x\n");
    expect(annotateRange(parsed, 99, 120)).toHaveLength(0);
  });
});

describe("parseUnifiedDiff meta classification", () => {
  const gitShowPayload = [
    "commit 9f8e7d6c5b4a (HEAD -> main, origin/main)",
    "Author: Ada Lovelace <ada@example.com>",
    "Date:   Mon Aug 24 10:00:00 2026 +0000",
    "",
    "    feat: add parser and drop legacy asset",
    "",
    "diff --git a/src/parser.rs b/src/parser.rs",
    "index e69de29..4b825dc 100644",
    "--- a/src/parser.rs",
    "+++ b/src/parser.rs",
    "@@ -1,2 +1,3 @@",
    " fn main() {",
    "+    parser::run();",
    " }",
    "diff --git a/src/legacy.rs b/src/legacy.rs",
    "deleted file mode 100644",
    "index 4b825dc..e69de29",
    "--- a/src/legacy.rs",
    "+++ /dev/null",
    "@@ -1 +0,0 @@",
    "-legacy",
    "diff --git a/logo.png b/logo.png",
    "similarity index 92%",
    "rename from assets/logo_old.png",
    "rename to logo.png",
    "diff --git a/diagram.png b/diagram.png",
    "index 1111111..2222222 100644",
    "Binary files a/diagram.png and b/diagram.png differ",
  ].join("\n");

  it("classifies commit metadata as meta instead of fake context rows", () => {
    const lines = parseUnifiedDiff(gitShowPayload);
    const indexRow = lines.find((l) => l.content.startsWith("index "));
    expect(indexRow?.type).toBe("meta");
    const deletedRow = lines.find((l) => l.content.startsWith("deleted file mode "));
    expect(deletedRow?.type).toBe("meta");
    const similarityRow = lines.find((l) => l.content.startsWith("similarity index "));
    expect(similarityRow?.type).toBe("meta");
    const renameFromRow = lines.find((l) => l.content.startsWith("rename from "));
    expect(renameFromRow?.type).toBe("meta");
    // Nothing that is metadata leaks into the context stream.
    const ctxContents = lines.filter((l) => l.type === "ctx").map((l) => l.content);
    expect(ctxContents).toEqual([" fn main() {", " }"]);
  });

  it("never counts meta or binary rows toward content stats", () => {
    const lines = parseUnifiedDiff(gitShowPayload);
    const contentRows = lines.filter((l) => l.type === "add" || l.type === "del" || l.type === "ctx");
    expect(contentRows.map((l) => l.content)).toEqual([
      " fn main() {",
      "+    parser::run();",
      " }",
      "-legacy",
    ]);
  });

  it("surfaces a distinct binary notice row for Binary files lines", () => {
    const lines = parseUnifiedDiff(gitShowPayload);
    const binary = lines.filter((l) => l.type === "binary").map((l) => l.content);
    expect(binary).toEqual(["Binary files a/diagram.png and b/diagram.png differ"]);
    expect(binary[0]).not.toContain("GIT binary patch");
  });

  it("classifies GIT binary patch payloads as binary notices too", () => {
    const lines = parseUnifiedDiff("diff --git a/a.bin b/a.bin\nGIT binary patch\nliteral 10\n");
    expect(lines[1].type).toBe("binary");
    expect(lines[2].type).toBe("meta");
  });

  it("keeps hunk line numbers intact across a meta boundary", () => {
    const twoFile = [
      "diff --git a/a.txt b/a.txt",
      "--- a/a.txt",
      "+++ b/a.txt",
      "@@ -1 +1 @@",
      "-one",
      "+uno",
      "diff --git a/b.txt b/b.txt",
      "--- a/b.txt",
      "+++ b/b.txt",
      "@@ -5 +5 @@",
      "+five",
    ].join("\n");
    const lines = parseUnifiedDiff(twoFile);
    const five = lines.find((l) => l.type === "add" && l.content === "+five");
    expect(five?.newNo).toBe(5);
  });
});

describe("filterFilePatch", () => {
  const multiFile = [
    "commit abcdef1234567890",
    "Author: Ada <ada@example.com>",
    "",
    "    subject line",
    "",
    "diff --git a/src/a.rs b/src/a.rs",
    "index aaa..bbb 100644",
    "--- a/src/a.rs",
    "+++ b/src/a.rs",
    "@@ -1 +1 @@",
    "-alpha",
    "+ALPHA",
    "diff --git a/src/b.ts b/src/b.ts",
    "index ccc..ddd 100644",
    "--- a/src/b.ts",
    "+++ b/src/b.ts",
    "@@ -1 +1 @@",
    "-beta",
    "+BETA",
    "diff --git a/old_name.txt b/new_name.txt",
    "similarity index 100%",
    "rename from old_name.txt",
    "rename to new_name.txt",
  ].join("\n");

  it("extracts exactly one file's patch with its metadata block", () => {
    const patch = filterFilePatch(multiFile, "src/b.ts");
    expect(patch).toContain("diff --git a/src/b.ts b/src/b.ts");
    expect(patch).toContain("+BETA");
    expect(patch).not.toContain("ALPHA");
    expect(patch).not.toContain("rename from");
    expect(patch.split("\n")[0]).toBe("diff --git a/src/b.ts b/src/b.ts");
  });

  it("matches pure renames by their new name", () => {
    expect(filterFilePatch(multiFile, "new_name.txt")).toContain("rename to new_name.txt");
    expect(filterFilePatch(multiFile, "old_name.txt")).toContain("rename from old_name.txt");
  });

  it("drops the git-show preamble before the first file header", () => {
    const patch = filterFilePatch(multiFile, "src/a.rs");
    expect(patch).not.toContain("Author:");
    expect(patch).not.toContain("subject line");
  });

  it("returns empty for paths absent from the diff and for empty input", () => {
    expect(filterFilePatch(multiFile, "src/missing.rs")).toBe("");
    expect(filterFilePatch("", "src/a.rs")).toBe("");
    expect(filterFilePatch(multiFile, "")).toBe("");
  });

  it("does not let one path shadow a longer sibling path", () => {
    const tricky = [
      "diff --git a/src/a.ts b/src/a.ts",
      "--- a/src/a.ts",
      "+++ b/src/a.ts",
      "@@ -1 +1 @@",
      "-x",
      "diff --git a/src/a.ts.bak b/src/a.ts.bak",
      "--- a/src/a.ts.bak",
      "+++ b/src/a.ts.bak",
      "@@ -1 +1 @@",
      "-y",
    ].join("\n");
    expect(filterFilePatch(tricky, "src/a.ts")).not.toContain(".bak");
    expect(filterFilePatch(tricky, "src/a.ts.bak")).toContain("-y");
  });

  it("handles quoted diff headers for paths with special characters", () => {
    const quoted =
      'diff --git "a/weird\tname" "b/weird\tname"\nindex aaa..bbb 100644\n--- "a/weird\tname"\n+++ "b/weird\tname"\n@@ -1 +1 @@\n-q\n+Q\n'.replace(
        /\t/g,
        " "
      );
    expect(filterFilePatch(quoted, "weird name")).toContain("+Q");
  });

  it("keeps timestamps after the path from breaking +++/--- matching", () => {
    const stamped = [
      "diff --git a/x.txt b/x.txt",
      "--- a/x.txt\t2026-01-01 00:00:00.000000000 +0000",
      "+++ b/x.txt\t2026-08-24 00:00:00.000000000 +0000",
      "@@ -1 +1 @@",
      "-t",
      "+T",
    ].join("\n");
    expect(filterFilePatch(stamped, "x.txt")).toContain("+T");
  });
});

describe("emptyDiffCopy", () => {
  it("explains clean merges instead of claiming nothing is selected", () => {
    const copy = emptyDiffCopy(true);
    expect(copy.title).toMatch(/merge/i);
    expect(copy.hint).toMatch(/cleanly|parent/i);
  });

  it("keeps the plain selection prompt for non-merges", () => {
    expect(emptyDiffCopy(false).title).toBe("No diff selected");
  });
});

describe("annotateUnifiedDiff whole-diff path", () => {
  it("still pairs adjacent delete/add lines end to end", () => {
    const lines = annotateUnifiedDiff("@@ -1 +1 @@\n-foo bar\n+foo baz\nctx\n");
    expect(lines[1].segments?.some((s) => s.kind === "Removed")).toBe(true);
    expect(lines[2].segments?.some((s) => s.kind === "Added")).toBe(true);
    expect(lines[3].segments).toBeUndefined();
  });
});
