import { describe, expect, it } from "vitest";
import {
  annotateRange,
  computeWordDiff,
  parseUnifiedDiff,
  type DiffSegment,
  type IntraLineDiff,
} from "./wordDiff";

// Mirrors of wordDiff.ts's private guards so boundary tests sit exactly on
// them; the module keeps these internal on purpose.
const MAX_TOKENS = 500;
const MAX_LINE_CHARS = 50_000;

/** True when `text` contains an unpaired UTF-16 surrogate code unit. */
function hasLoneSurrogate(text: string): boolean {
  for (let i = 0; i < text.length; i += 1) {
    const unit = text.charCodeAt(i);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = i + 1 < text.length ? text.charCodeAt(i + 1) : 0;
      if (next < 0xdc00 || next > 0xdfff) return true;
      i += 1;
    } else if (unit >= 0xdc00 && unit <= 0xdfff) {
      return true;
    }
  }
  return false;
}

/**
 * The renderer concatenates segment texts back into display lines, so every
 * char of `oldLine` must be covered by Equal/Removed segments and every char
 * of `newLine` by Equal/Added segments. Returns the diff for further asserts.
 */
function expectLossless(oldLine: string, newLine: string): IntraLineDiff {
  const diff = computeWordDiff(oldLine, newLine);
  expect(
    diff.original_segments.every((s) => s.kind === "Equal" || s.kind === "Removed")
  ).toBe(true);
  expect(diff.modified_segments.every((s) => s.kind === "Equal" || s.kind === "Added")).toBe(
    true
  );
  expect(diff.original_segments.map((s) => s.text).join("")).toBe(oldLine);
  expect(diff.modified_segments.map((s) => s.text).join("")).toBe(newLine);
  return diff;
}

/** mergeConsecutive guarantees no two adjacent segments share a kind. */
function expectMerged(segments: DiffSegment[]): void {
  for (let i = 1; i < segments.length; i += 1) {
    expect(segments[i].kind).not.toBe(segments[i - 1].kind);
  }
}

describe("computeWordDiff stress: degenerate inputs", () => {
  it("handles both sides empty", () => {
    const diff = computeWordDiff("", "");
    expect(diff.original_segments).toEqual([{ kind: "Equal", text: "" }]);
    expect(diff.modified_segments).toEqual([{ kind: "Equal", text: "" }]);
  });

  it("handles one side empty in both directions", () => {
    const added = computeWordDiff("", "brand new");
    expect(added.original_segments).toEqual([]);
    expect(added.modified_segments).toEqual([{ kind: "Added", text: "brand new" }]);

    const removed = computeWordDiff("gone line", "");
    expect(removed.original_segments).toEqual([{ kind: "Removed", text: "gone line" }]);
    expect(removed.modified_segments).toEqual([]);
  });

  it("returns a single Equal pair for identical strings longer than MAX_LINE_CHARS", () => {
    // The equality short-circuit runs BEFORE the size guard, so huge
    // identical lines stay Equal instead of degrading to a whole-line swap.
    const huge = "x".repeat(MAX_LINE_CHARS + 10);
    const diff = computeWordDiff(huge, huge);
    expect(diff.original_segments).toEqual([{ kind: "Equal", text: huge }]);
    expect(diff.modified_segments).toEqual([{ kind: "Equal", text: huge }]);
  });

  it("keeps CRLF vs LF differences reconstructable", () => {
    expectLossless("same words\r\n", "same words\n");
    const diff = computeWordDiff("same words\r\n", "same words\n");
    expect(diff.original_segments.some((s) => s.kind === "Removed" && s.text.includes("\r"))).toBe(
      true
    );
  });
});

describe("computeWordDiff stress: astral-plane characters", () => {
  const pairs: Array<[string, string]> = [
    ["fix 🎉 parser", "fix 🎉 parser!"],
    ["🎉🎉🎉 party", "🎊🎉🎉 party"],
    ["日本語のコメント here", "日本語のコメント here2"],
    ["a🎉b c🎉d", "ab c🎉d🎉"],
    ["🎉".repeat(200) + " end", "🎉".repeat(199) + " end"],
  ];

  it("never emits a lone surrogate in any segment", () => {
    for (const [oldLine, newLine] of pairs) {
      const diff = computeWordDiff(oldLine, newLine);
      for (const seg of [...diff.original_segments, ...diff.modified_segments]) {
        expect(hasLoneSurrogate(seg.text)).toBe(false);
        // Round-trip via the iterator proves every surrogate is paired.
        expect([...seg.text].join("")).toBe(seg.text);
      }
      expectLossless(oldLine, newLine);
    }
  });

  it("survives astral chars sitting exactly on token class boundaries", () => {
    // Word/non-word alternation around each half of a surrogate pair would be
    // the classic slicing bug; the tokenizer walks code units but surrogate
    // halves share a class so they must always land in one token together.
    const oldLine = "x🎉x🎉x";
    const newLine = "xx🎉x";
    const diff = expectLossless(oldLine, newLine);
    for (const seg of [...diff.original_segments, ...diff.modified_segments]) {
      expect(hasLoneSurrogate(seg.text)).toBe(false);
    }
  });
});

describe("computeWordDiff stress: guard boundaries", () => {
  function wideLine(chars: number, marker = ""): string {
    // Word/space alternation forces many tokens so the token cap is what
    // binds, not the char cap.
    const base = "ab ".repeat(Math.ceil(chars / 3)).slice(0, chars);
    return marker ? marker + base.slice(marker.length) : base;
  }

  it("tokenizes a line of exactly MAX_LINE_CHARS instead of short-circuiting", () => {
    const oldLine = wideLine(MAX_LINE_CHARS);
    const newLine = wideLine(MAX_LINE_CHARS, "Z");
    const diff = expectLossless(oldLine, newLine);
    // Tokenized path: multiple segments, none spanning the whole line (the
    // whole-line-swap guard would have produced exactly one Removed).
    expect(diff.original_segments.length).toBeGreaterThan(1);
    expect(diff.original_segments[0].text.length).toBeLessThan(MAX_LINE_CHARS);
    expect(diff.modified_segments.some((s) => s.kind === "Added")).toBe(true);
  });

  it("whole-line swaps at MAX_LINE_CHARS + 1 without tokenizing", () => {
    const oldLine = wideLine(MAX_LINE_CHARS + 1);
    const newLine = wideLine(MAX_LINE_CHARS + 1, "Z");
    const diff = computeWordDiff(oldLine, newLine);
    expect(diff.original_segments).toEqual([{ kind: "Removed", text: oldLine }]);
    expect(diff.modified_segments).toEqual([{ kind: "Added", text: newLine }]);
  });

  it("handles exactly MAX_TOKENS tokens through the normal path", () => {
    // n single-letter words joined by single spaces = 2n-1 tokens; a trailing
    // space makes the 500th.
    const wordCount = MAX_TOKENS / 2;
    const oldLine = `${Array.from({ length: wordCount }, (_, i) => String.fromCharCode(97 + (i % 26))).join(" ")} `;
    expectLossless(oldLine, `${oldLine}z`);
  });

  it("routes the tail beyond MAX_TOKENS into one remainder token", () => {
    // MAX_TOKENS/2 + 1 words = MAX_TOKENS + 1 natural tokens; the tokenizer
    // caps at MAX_TOKENS and pushes each line's remainder as a single tail
    // token. The two tails differ, so exactly one Removed/Added pair appears
    // at the end while everything before reconstructs byte-for-byte.
    const words = Array.from(
      { length: MAX_TOKENS / 2 + 1 },
      (_, i) => String.fromCharCode(97 + (i % 26))
    );
    const oldLine = words.join(" ");
    const newLine = `${words.join(" ")} tail`;
    const diff = expectLossless(oldLine, newLine);
    expect(diff.original_segments.filter((s) => s.kind === "Removed")).toHaveLength(1);
    const added = diff.modified_segments.filter((s) => s.kind === "Added");
    expect(added).toHaveLength(1);
    expect(added[0].text.includes("tail")).toBe(true);
    // The tail segment is last on both sides.
    expect(diff.original_segments[diff.original_segments.length - 1].kind).toBe("Removed");
    expect(diff.modified_segments[diff.modified_segments.length - 1].kind).toBe("Added");
  });

  it("reconstructs whitespace-only changes byte-exactly", () => {
    // `ignoreWhitespace` is a git-layer (-w) refetch flag, never a parameter
    // here; the invariant that matters regardless of the toggle is that
    // whitespace-only pairs survive with their exact bytes.
    expectLossless("foo     bar", "foo\tbar");
    expectLossless("indent    ", "indent ");
    expectLossless("  a  b  ", " a b ");
    expectLossless("\t\tx", "    x");
  });
});

describe("computeWordDiff stress: segment sequences", () => {
  it("produces interleaved runs with no duplicated adjacency and full coverage", () => {
    const oldLine = "one two three four five six seven eight";
    const newLine = "ONE two THREE four five SIX seven nine";
    const diff = expectLossless(oldLine, newLine);
    expectMerged(diff.original_segments);
    expectMerged(diff.modified_segments);
    // At least two separate Removed islands separated by Equal runs.
    const removedRuns = diff.original_segments.filter((s) => s.kind === "Removed");
    expect(removedRuns.length).toBeGreaterThanOrEqual(2);
  });

  it("keeps a zero-shared-token rewrite as one Removed plus one Added", () => {
    // Single tokens with nothing in common (not even whitespace): LCS is
    // empty, so the whole lines collapse into one swap pair.
    const diff = computeWordDiff("alphabeta", "omegapsichi");
    expect(diff.original_segments).toEqual([{ kind: "Removed", text: "alphabeta" }]);
    expect(diff.modified_segments).toEqual([{ kind: "Added", text: "omegapsichi" }]);
  });

  it("keeps shared separator tokens as Equal islands in a word-level rewrite", () => {
    // Pinned: LCS runs on TOKENS, and single spaces recur across both lines,
    // so a full word rewrite still shows Equal(" ") islands between Removed
    // runs. Correct per the tokenizer's contract — pinned so renderers don't
    // assume monotone Removed runs.
    const diff = computeWordDiff("alpha beta gamma", "omega psi chi");
    expectLossless("alpha beta gamma", "omega psi chi");
    expectMerged(diff.original_segments);
    expect(diff.original_segments.some((s) => s.kind === "Equal")).toBe(true);
  });
});

describe("parseUnifiedDiff + annotateRange stress: 200-line document", () => {
  it("annotates every changed pair in one pass well under 2s", () => {
    const lines: string[] = [];
    for (let i = 0; i < 200; i += 1) {
      const body = `line ${i}: ${"lorem ipsum dolor sit amet consectetur".slice(0, 20 + (i % 9))} value=${i}`;
      if (i % 7 === 0) {
        lines.push(`-${body}`, `+${body} EDITED`);
      } else {
        lines.push(` ${body}`);
      }
    }
    const raw = `@@ -1,200 +1,200 @@\n${lines.join("\n")}`;
    const parsed = parseUnifiedDiff(raw);

    const startedAt = performance.now();
    const annotated = annotateRange(parsed, 0, parsed.length);
    const elapsedMs = performance.now() - startedAt;

    expect(elapsedMs).toBeLessThan(2_000);
    const dels = annotated.filter((l) => l.type === "del");
    const adds = annotated.filter((l) => l.type === "add");
    expect(dels).toHaveLength(29);
    expect(adds).toHaveLength(29);
    for (const del of dels) {
      expect(del.segments?.length).toBeGreaterThan(0);
    }
    for (const add of adds) {
      expect(add.segments?.length).toBeGreaterThan(0);
    }
  });
});
