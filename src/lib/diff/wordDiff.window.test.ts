import { describe, expect, it } from "vitest";
import {
  WORD_DIFF_CELL_BUDGET,
  WORD_DIFF_PAIR_HALO,
  annotateAround,
  annotateRange,
  parseUnifiedDiff,
  replacementBlock,
  replacementBlockBounds,
  type AnnotatedDiffLine,
} from "./wordDiff";

/** One hunk of `pairs` paired del/add lines, `del` run first, then `add` run. */
function replacementDiff(pairs: number, width = 40, extraAdds = 0, extraDels = 0): string {
  const out = [`@@ -1,${pairs + extraDels} +1,${pairs + extraAdds} @@`];
  const pad = "x".repeat(width);
  for (let i = 0; i < pairs + extraDels; i += 1) out.push(`-old ${i} ${pad} end`);
  for (let i = 0; i < pairs + extraAdds; i += 1) out.push(`+new ${i} ${pad} end`);
  return out.join("\n") + "\n";
}

const withSegments = (lines: AnnotatedDiffLine[]) =>
  lines.filter((l) => l.segments !== undefined).length;

describe("annotateRange characterization (unmodified behaviour)", () => {
  it("annotates every pair of a block it is handed", () => {
    const lines = parseUnifiedDiff(replacementDiff(50));
    annotateRange(lines, 0, lines.length);
    expect(withSegments(lines)).toBe(100);
  });

  it("pairs del[k] with add[k] and leaves the surplus plain", () => {
    const lines = parseUnifiedDiff(replacementDiff(3, 5, 2, 0));
    annotateRange(lines, 0, lines.length);
    const adds = lines.filter((l) => l.type === "add");
    expect(adds.map((l) => l.segments !== undefined)).toEqual([true, true, true, false, false]);
  });

  it("reports identical bounds from any row of the block", () => {
    const lines = parseUnifiedDiff(replacementDiff(4));
    const expected = replacementBlockBounds(lines, 1);
    for (let i = 1; i < lines.length; i += 1) {
      expect(replacementBlockBounds(lines, i)).toEqual(expected);
    }
  });
});

/** Varied multi-kilobyte line so every pair is a real, non-trivial LCS. */
function bigLine(seed: number, words: number): string {
  const parts: string[] = [];
  for (let i = 0; i < words; i += 1) parts.push(`w${(seed * 7 + i * 13) % 97}`);
  return parts.join(" ");
}

function bigBlockDiff(pairs: number, words: number): string {
  const out = [`@@ -1,${pairs} +1,${pairs} @@`];
  for (let i = 0; i < pairs; i += 1) out.push(`-${bigLine(i, words)}`);
  for (let i = 0; i < pairs; i += 1) out.push(`+${bigLine(i + 1, words)}`);
  return out.join("\n") + "\n";
}

const maxWindowPairs = 2 * WORD_DIFF_PAIR_HALO + 1;

describe("annotateAround bounds the pairs it annotates", () => {
  it("annotates a window, not 2000 pairs, for the first visible row", () => {
    // ~1000 words of ~4 chars: 3-5 KB per line, under MAX_LINE_CHARS.
    const lines = parseUnifiedDiff(bigBlockDiff(2000, 1000));
    expect(lines[1].content.length).toBeGreaterThan(3000);
    expect(lines[1].content.length).toBeLessThan(50_000);
    annotateAround(lines, 1);
    const annotated = withSegments(lines);
    expect(annotated).toBeGreaterThan(0);
    expect(annotated).toBeLessThanOrEqual(2 * maxWindowPairs);
    expect(lines[1].segments).toBeDefined();
    expect(lines[2000 + 1].segments).toBeDefined();
    expect(lines[1000].segments).toBeUndefined();
    expect(lines[3000].segments).toBeUndefined();
  });

  it("caps the LCS cells one call may spend, but always annotates the target pair", () => {
    // Token-capped lines: each pair costs the full 501x501 table.
    const lines = parseUnifiedDiff(bigBlockDiff(200, 1000));
    annotateAround(lines, 100);
    const pairs = withSegments(lines) / 2;
    expect(pairs).toBeGreaterThanOrEqual(1);
    expect(pairs).toBeLessThanOrEqual(Math.ceil(WORD_DIFF_CELL_BUDGET / (501 * 501)) + 1);
    expect(lines[100].segments).toBeDefined();
    expect(lines[200 + 100].segments).toBeDefined();
  });

  it("annotates every row a renderer visits, in any order", () => {
    const lines = parseUnifiedDiff(bigBlockDiff(120, 40));
    for (let i = lines.length - 1; i >= 1; i -= 1) {
      annotateAround(lines, i);
      expect(lines[i].segments).toBeDefined();
    }
  });

  it("still annotates a one-pair hunk with word segments", () => {
    const lines = parseUnifiedDiff("@@ -1 +1 @@\n-let count = 42;\n+let count = 100;\n");
    annotateAround(lines, 1);
    expect(lines[1].segments?.some((s) => s.kind === "Removed" && s.text === "42")).toBe(true);
    expect(lines[2].segments?.some((s) => s.kind === "Added" && s.text === "100")).toBe(true);
  });

  it("matches annotateRange exactly for a block inside the window", () => {
    const raw = replacementDiff(10, 12);
    const a = parseUnifiedDiff(raw);
    const b = parseUnifiedDiff(raw);
    annotateRange(a, 0, a.length);
    annotateAround(b, 5);
    expect(b).toEqual(a);
  });

  it("is idempotent and keeps segment identity on repeat calls", () => {
    const lines = parseUnifiedDiff(replacementDiff(5, 8));
    annotateAround(lines, 2);
    const first = lines[2].segments;
    annotateAround(lines, 2);
    expect(lines[2].segments).toBe(first);
  });

  it("reuses a caller-supplied block and yields the same result", () => {
    const raw = replacementDiff(30, 8);
    const a = parseUnifiedDiff(raw);
    const b = parseUnifiedDiff(raw);
    annotateAround(a, 15);
    annotateAround(b, 15, replacementBlock(b, 15) ?? undefined);
    expect(b).toEqual(a);
  });

  it("ignores context, header and out-of-range rows", () => {
    const lines = parseUnifiedDiff("@@ -1,3 +1,3 @@\n a\n-b\n+c\n d\n");
    for (const i of [0, 1, 4, -1, 99]) annotateAround(lines, i);
    expect(withSegments(lines)).toBe(0);
    expect(replacementBlock(lines, 1)).toBeNull();
  });
});

describe("annotateAround adversarial inputs", () => {
  it("handles empty lines on either side", () => {
    const lines = parseUnifiedDiff("@@ -1,2 +1,2 @@\n-\n-foo\n+bar\n+\n");
    annotateAround(lines, 1);
    annotateAround(lines, 2);
    expect(lines.filter((l) => l.type !== "hdr").every((l) => l.segments !== undefined)).toBe(
      true
    );
    expect(lines[1].segments?.map((s) => s.text).join("")).toBe("");
  });

  it("handles a single huge line past MAX_LINE_CHARS as a plain replace", () => {
    const lines = parseUnifiedDiff(`@@ -1 +1 @@\n-${"a".repeat(60_000)}\n+${"b".repeat(60_000)}\n`);
    annotateAround(lines, 1);
    expect(lines[1].segments).toEqual([{ kind: "Removed", text: "a".repeat(60_000) }]);
  });

  it("handles many short lines", () => {
    const lines = parseUnifiedDiff(replacementDiff(5000, 1));
    annotateAround(lines, 2500);
    expect(withSegments(lines)).toBeLessThanOrEqual(2 * maxWindowPairs);
    expect(lines[2500].segments).toBeDefined();
  });

  it("keeps multibyte and emoji text lossless", () => {
    const lines = parseUnifiedDiff("@@ -1 +1 @@\n-héllo 👨‍👩‍👧 wörld\n+héllo 🎉 wörld\n");
    annotateAround(lines, 1);
    expect(lines[1].segments?.map((s) => s.text).join("")).toBe("héllo 👨‍👩‍👧 wörld");
    expect(lines[2].segments?.map((s) => s.text).join("")).toBe("héllo 🎉 wörld");
  });

  it("keeps CRLF carriage returns in the segments", () => {
    const lines = parseUnifiedDiff("@@ -1 +1 @@\n-foo bar\r\n+foo baz\r\n");
    annotateAround(lines, 1);
    expect(lines[1].segments?.map((s) => s.text).join("")).toBe("foo bar\r");
    expect(lines[2].segments?.map((s) => s.text).join("")).toBe("foo baz\r");
  });

  it("does not pair across the no-newline marker and keeps its flag", () => {
    const lines = parseUnifiedDiff("@@ -1 +1 @@\n-foo\n\\ No newline at end of file\n+bar\n");
    annotateAround(lines, 1);
    annotateAround(lines, 3);
    expect(lines[1].noNewline).toBe(true);
    expect(lines[1].segments).toBeUndefined();
    expect(lines[3].segments).toBeUndefined();
  });

  it("leaves surplus rows of unequal runs plain without throwing", () => {
    const lines = parseUnifiedDiff(replacementDiff(3, 4, 4, 0));
    for (let i = 1; i < lines.length; i += 1) annotateAround(lines, i);
    const adds = lines.filter((l) => l.type === "add");
    expect(adds.map((l) => l.segments !== undefined)).toEqual([
      true, true, true, false, false, false, false,
    ]);
    const more = parseUnifiedDiff(replacementDiff(3, 4, 0, 4));
    for (let i = 1; i < more.length; i += 1) annotateAround(more, i);
    expect(more.filter((l) => l.type === "del").filter((l) => l.segments).length).toBe(3);
  });

  it("bounds a pathological LCS input (alternating tokens, 500-token cap)", () => {
    const a = Array.from({ length: 800 }, (_, i) => (i % 2 ? "x" : "y")).join(" ");
    const b = Array.from({ length: 800 }, (_, i) => (i % 3 ? "y" : "x")).join(" ");
    const out = ["@@ -1,300 +1,300 @@"];
    for (let i = 0; i < 300; i += 1) out.push(`-${a}${i}`);
    for (let i = 0; i < 300; i += 1) out.push(`+${b}${i}`);
    const lines = parseUnifiedDiff(out.join("\n") + "\n");
    annotateAround(lines, 150);
    expect(withSegments(lines) / 2).toBeLessThanOrEqual(
      Math.ceil(WORD_DIFF_CELL_BUDGET / (501 * 501)) + 1
    );
    expect(lines[150].segments).toBeDefined();
  });
});
