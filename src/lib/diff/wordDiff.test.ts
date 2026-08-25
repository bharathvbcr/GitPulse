import { describe, expect, it } from "vitest";
import {
  annotateRange,
  annotateUnifiedDiff,
  computeWordDiff,
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

  it("returns an empty slice for out-of-bounds ranges", () => {
    const parsed = parseUnifiedDiff("+x\n");
    expect(annotateRange(parsed, 99, 120)).toHaveLength(0);
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
