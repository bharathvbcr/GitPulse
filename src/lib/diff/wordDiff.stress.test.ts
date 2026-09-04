import { describe, expect, it } from "vitest";
import { STRESS_TIMEOUT_MS, expectWithinBudget } from "../__tests__/perfBudget";
import {
  annotateRange,
  classifyMetaLine,
  computeWordDiff,
  filterFilePatch,
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

    expectWithinBudget(elapsedMs, 400, "wordDiff stress");
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
  }, STRESS_TIMEOUT_MS);
});

// ---------------------------------------------------------------------------
// Wave-2 adversarial additions: parseUnifiedDiff / filterFilePatch attacks.
// Every expectation below was pinned against the current wordDiff.ts
// implementation; anything the code gets wrong is marked it.fails with a BUG.
// ---------------------------------------------------------------------------

describe("parseUnifiedDiff stress: header degeneracy", () => {
  it("reduces empty and nullish payloads to zero rows", () => {
    // CHANGED (bug fix): "".split("\n") used to yield [""], and the empty
    // string fell into the outside-hunk else branch → one {type:"meta",
    // content:""} phantom row. That row made the UI's EmptyState branch
    // unreachable and inflated every count. An empty diff is zero rows.
    expect(parseUnifiedDiff("")).toEqual([]);
    expect(parseUnifiedDiff(undefined as unknown as string)).toEqual([]);
    expect(parseUnifiedDiff(null as unknown as string)).toEqual([]);
  });

  it("classifies a lone '@' outside a hunk as meta, not content", () => {
    const rows = parseUnifiedDiff("@");
    expect(rows).toHaveLength(1);
    expect(rows[0].type).toBe("meta");
  });

  it("keeps a lone '@' inside a hunk as context that advances both counters", () => {
    // Pinned characterization: "@" satisfies PATCH_BODY_PREFIX_RE (^[-+ \\@]),
    // so once inHunk is true it is treated as real file content and bumps
    // oldNo/newNo like any other context line. Harmless for git-produced
    // patches (a body line can never be bare "@"), pinned so hostile input
    // cannot crash or stall numbering here.
    const rows = parseUnifiedDiff("@@ -10,2 +10,2 @@\n@\n ctx");
    expect(rows.map((r) => r.type)).toEqual(["hdr", "ctx", "ctx"]);
    expect(rows[1].oldNo).toBe(10);
    expect(rows[1].newNo).toBe(10);
    expect(rows[2].oldNo).toBe(11);
    expect(rows[2].newNo).toBe(11);
  });

  it("treats any '@@'-prefixed line as a header even when HUNK_RE cannot parse numbers", () => {
    // CHANGED (bug fix): "@@" alone or "@@ garbage" becomes a hdr row with
    // inHunk set, but the counters are now RESET instead of silently
    // inheriting the previous hunk's numbering. Body rows after an
    // unparseable header carry undefined numbers — never a cross-file lie.
    const rows = parseUnifiedDiff("@@ -5,1 +5,1 @@\n-a\n@@\n+b");
    expect(rows.map((r) => r.type)).toEqual(["hdr", "del", "hdr", "add"]);
    expect(rows[1].oldNo).toBe(5);
    expect(rows[3].oldNo).toBeUndefined();
    expect(rows[3].newNo).toBeUndefined();
  });

  it("accepts git's padded hunk headers through the relaxed HUNK_RE", () => {
    const rows = parseUnifiedDiff("@@  -12,3 +14,4 @@ fn padded()\n ctx\n+added\n");
    expect(rows[0].type).toBe("hdr");
    expect(rows[1]).toMatchObject({ type: "ctx", oldNo: 12, newNo: 14 });
    expect(rows[2]).toMatchObject({ type: "add", newNo: 15 });
  });

  it("never throws on '@@' with NUL bytes or lone surrogates embedded", () => {
    expect(() => parseUnifiedDiff("@@ -\u0000 +\u0000 @@\n+\u0000x")).not.toThrow();
    expect(() => parseUnifiedDiff("+\uD800 next")).not.toThrow();
    const rows = parseUnifiedDiff("+\uD800 tail");
    expect(rows[0].type).toBe("add");
    // The unpaired surrogate must survive byte-for-byte into the row content.
    expect(rows[0].content).toBe("+\uD800 tail");
  });
});

describe("computeWordDiff stress: lone-surrogate inputs", () => {
  it("reconstructs lines built from unpaired surrogates losslessly", () => {
    const diff = computeWordDiff("\uD800abc", "\uD800abd");
    expect(diff.original_segments.map((s) => s.text).join("")).toBe("\uD800abc");
    expect(diff.modified_segments.map((s) => s.text).join("")).toBe("\uD800abd");
  });
});

describe("parseUnifiedDiff stress: malformed hunk counts", () => {
  it("emits exactly the body lines when counts over-declare", () => {
    // Header claims 3 new lines but only one follows. The parser never reads
    // the counts beyond the start offsets, so no phantom rows appear.
    const rows = parseUnifiedDiff("@@ -1,1 +1,3 @@\n+only");
    expect(rows.map((r) => r.type)).toEqual(["hdr", "add"]);
    expect(rows[1].newNo).toBe(1);
  });

  it("keeps classifying extra body lines that overrun the declared count", () => {
    // Two deletions where the header declares one: both must stay del rows
    // and advance oldNo monotonically — count under-run must not silently
    // re-type trailing body lines as context or metadata.
    const rows = parseUnifiedDiff("@@ -7,1 +7,0 @@\n-first\n-second\n");
    expect(rows.filter((r) => r.type === "del").map((r) => r.oldNo)).toEqual([7, 8]);
  });
});

describe("parseUnifiedDiff stress: CR handling", () => {
  it("treats a CR-only payload as ONE line, not many", () => {
    // split("\n") never splits on \r: classic-Mac line endings yield a single
    // giant row whose type comes from its first character. Content keeps the
    // type prefix (annotateRange strips it via slice(1)). No hang, no throw.
    const rows = parseUnifiedDiff("-a\rb\rc");
    expect(rows).toHaveLength(1);
    expect(rows[0].type).toBe("del");
    expect(rows[0].content).toBe("-a\rb\rc");
  });

  it("parses CRLF hunk headers because HUNK_RE is end-unanchored", () => {
    const rows = parseUnifiedDiff("@@ -3,1 +3,1 @@\r\n ctx\r\n-old\r\n+new\r\n");
    expect(rows.map((r) => r.type)).toEqual(["hdr", "ctx", "del", "add"]);
    expect(rows[0].content.endsWith("\r")).toBe(true);
    expect(rows[1].oldNo).toBe(3);
    expect(rows[3].content).toBe("+new\r");
    // The leading ctx already consumed one number from each side.
    expect(rows[3].newNo).toBe(4);
    // CHANGED (bug fix): the final "" split artifact from the trailing \n
    // used to land inside the hunk as one phantom ctx row bumping both
    // counters. It was the line terminator, not a line, and is now dropped.
    expect(rows).toHaveLength(4);
  });

  it("handles mixed LF and CRLF endings in one payload without throwing", () => {
    const raw = "@@ -1,2 +1,2 @@\n keep\r\n-del\r\n+add";
    const rows = parseUnifiedDiff(raw);
    expect(rows.map((r) => r.type)).toEqual(["hdr", "ctx", "del", "add"]);
    expect(rows[1]).toMatchObject({ oldNo: 1, newNo: 1 });
    expect(rows[2].oldNo).toBe(2);
    expect(rows[3].newNo).toBe(2);
  });
});

describe("parseUnifiedDiff stress: performance bounds", () => {
  it("parses a 1MB single-line addition in under 2000ms", () => {
    const huge = "+" + "x".repeat(1_000_000);
    console.time("parseUnifiedDiff:1MB-line");
    const startedAt = performance.now();
    const rows = parseUnifiedDiff(huge);
    const elapsedMs = performance.now() - startedAt;
    console.timeEnd("parseUnifiedDiff:1MB-line");
    expect(rows).toHaveLength(1);
    expect(rows[0].type).toBe("add");
    expectWithinBudget(elapsedMs, 400, "wordDiff stress");
  }, STRESS_TIMEOUT_MS);

  it("parses a 50k-line payload in under 3000ms", () => {
    const lines: string[] = ["@@ -1,25000 +1,25000 @@"];
    for (let i = 0; i < 25_000; i += 1) {
      lines.push(`-${"lorem ipsum dolor ".repeat(4)}${i}`, `+${"lorem ipsum dolor ".repeat(4)}EDITED`);
    }
    const raw = lines.join("\n");
    const startedAt = performance.now();
    const rows = parseUnifiedDiff(raw);
    const elapsedMs = performance.now() - startedAt;
    expect(rows).toHaveLength(50_001);
    expect(rows.filter((r) => r.type === "add")).toHaveLength(25_000);
    expectWithinBudget(elapsedMs, 600, "wordDiff pathological");
  }, STRESS_TIMEOUT_MS);
});

describe("parseUnifiedDiff stress: --- / +++ inside hunk bodies", () => {
  it("types a deleted line whose content starts with '--' as del, not hdr", () => {
    // Removing a Markdown horizontal rule / front-matter fence renders as a
    // body line starting with "---". Git disambiguates via the hunk counts;
    // header classification is gated on being outside any hunk body, so the
    // deletion stays a del row and annotateRange can pair it.
    const rows = parseUnifiedDiff(
      ["@@ -1,3 +1,2 @@", " title", "---", " body", "+kept"].join("\n")
    );
    expect(rows.map((r) => r.type)).toEqual(["hdr", "ctx", "del", "ctx", "add"]);
  });

  it("types an added line whose content starts with '++' as add, not hdr", () => {
    // Adding a line whose CONTENT begins with "++" (e.g. a diff sample or
    // C++ pre/post-increment sample) renders the patch line "+++ ...",
    // which stays an add row inside the hunk body.
    const rows = parseUnifiedDiff(
      ["@@ -1,2 +1,3 @@", " title", "+++ bold intro", " body"].join("\n")
    );
    expect(rows.map((r) => r.type)).toEqual(["hdr", "ctx", "add", "ctx"]);
  });

  it("still routes real per-file headers outside hunks to hdr", () => {
    // Guarding the two BUG pins above: outside any hunk the classic header
    // pair must keep working.
    const rows = parseUnifiedDiff("--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-a\n+b");
    expect(rows.slice(0, 2).map((r) => r.type)).toEqual(["hdr", "hdr"]);
    expect(rows.slice(3).map((r) => r.type)).toEqual(["del", "add"]);
  });
});

describe("parseUnifiedDiff stress: \\ No newline markers", () => {
  it("keeps consecutive no-newline markers as hdr rows between del/add pairs", () => {
    const rows = parseUnifiedDiff(
      [
        "@@ -1,2 +1,2 @@",
        " fn a() {}",
        "-fn b()",
        "\\ No newline at end of file",
        "+fn b() {}",
        "\\ No newline at end of file",
      ].join("\n")
    );
    expect(rows.map((r) => r.type)).toEqual([
      "hdr", "ctx", "del", "hdr", "add", "hdr",
    ]);
    // The markers must not disturb the counters feeding neighboring rows.
    expect(rows[2].oldNo).toBe(2);
    expect(rows[4].newNo).toBe(2);
    // Each marker flags exactly its own preceding row.
    expect(rows[2].noNewline).toBe(true);
    expect(rows[4].noNewline).toBe(true);
  });

  it("never lets a marker flag a row across an intervening row type", () => {
    const rows = parseUnifiedDiff(
      ["@@ -1,3 +1,3 @@", "-gone", "index 111..222 100644", "+born", "\\ No newline at end of file"].join("\n")
    );
    expect(rows[1].type).toBe("del");
    expect(rows[1].noNewline).toBeUndefined();
    // The meta row between del and add breaks adjacency; only "+born"
    // (immediately before the marker) gets flagged.
    expect(rows[2].type).toBe("meta");
    expect(rows[3].noNewline).toBe(true);
  });
});

describe("parseUnifiedDiff stress: GIT binary payload sections", () => {
  it("swallows hostile-looking base85 payloads without phantom diff rows", () => {
    // Adversarial payload: every line begins with a patch-body character.
    // Before the binary-section rule these became add/del/meta soup and
    // corrupted numbering for any file that followed.
    const raw = [
      "diff --git a/blob.bin b/blob.bin",
      "index 1111111..2222222 100644",
      "GIT binary patch",
      "literal 96",
      "-cmZ<+q0u~000000",
      "+cmZ>+r1v!111111",
      "\\ No newline at end of file",
      "@@ -1 +1 @@",
      "",
      "diff --git a/after.txt b/after.txt",
      "--- a/after.txt",
      "+++ b/after.txt",
      "@@ -7 +7 @@",
      "+after edit",
    ].join("\n");
    const rows = parseUnifiedDiff(raw);
    // Everything from GIT binary patch up to (not including) the next
    // diff --git is binary chrome — even lines starting with '-', '\\', '@@'.
    const start = rows.findIndex((l) => l.content === "GIT binary patch");
    const end = rows.findIndex((l) => l.content === "diff --git a/after.txt b/after.txt");
    for (let i = start; i < end; i += 1) {
      expect(rows[i].type).toBe("binary");
    }
    expect(rows.filter((l) => l.type === "add" || l.type === "del")).toHaveLength(1);
    expect(rows.at(-1)).toMatchObject({ type: "add", newNo: 7 });
  });
});

describe("parseUnifiedDiff stress: commit meta interleaved mid-hunk", () => {
  it("emits meta rows mid-hunk but keeps numbering alive across them", () => {
    // `git show` noise landing inside a body must become a meta row while the
    // surrounding +/-/space lines continue counting from the same header.
    // Deletions only advance oldNo and additions only newNo, so after "-a"
    // (oldNo 20→21) the following "+b" still carries the untouched newNo 20.
    const rows = parseUnifiedDiff(
      ["@@ -20,3 +20,3 @@", "-a", "index 111..222 100644", "+b"].join("\n")
    );
    expect(rows.map((r) => r.type)).toEqual(["hdr", "del", "meta", "add"]);
    expect(rows[1].oldNo).toBe(20);
    expect(rows[3].newNo).toBe(20);
  });

  it("closes the hunk on 'diff --git' so prose after files stays meta", () => {
    const rows = parseUnifiedDiff(
      ["diff --git a/x b/x", "@@ -1 +1 @@", "-a", "+b", "diff --git a/y b/y", "trailing prose"]
        .join("\n")
    );
    expect(rows.at(-1)!.type).toBe("meta");
  });
});

describe("classifyMetaLine vs filterFilePatch agreement", () => {
  const twoFilePayload = [
    "diff --git a/src/a.rs b/src/a.rs",
    "index 111..222 100644",
    "--- a/src/a.rs",
    "+++ b/src/a.rs",
    "@@ -1 +1 @@",
    "-a",
    "+b",
    "diff --git a/sp ace/uniﬁé😀.txt b/sp ace/uniﬁé😀.txt",
    "index 333..444 100644",
    "--- a/sp ace/uniﬁé😀.txt",
    "+++ b/sp ace/uniﬁé😀.txt",
    "@@ -1 +1 @@",
    "-old 😀",
    "+new 😀",
  ].join("\n");

  it("extracts exactly the matching section and nothing else", () => {
    const patch = filterFilePatch(twoFilePayload, "sp ace/uniﬁé😀.txt");
    expect(patch).toContain("diff --git a/sp ace/uniﬁé😀.txt");
    expect(patch).toContain("+new 😀");
    expect(patch).not.toContain("src/a.rs");
    // Re-parsing the kept section standalone: its own header block rides
    // along (diff --git + index classify as meta, ---/+++ as hdr), and the
    // hunk body stays exactly del/add — nothing from the OTHER file leaks in.
    const types = parseUnifiedDiff(patch).map((r) => r.type);
    expect(types).toEqual(["meta", "meta", "hdr", "hdr", "hdr", "del", "add"]);
  });

  it("returns empty string — not a throw — when no section matches", () => {
    expect(filterFilePatch(twoFilePayload, "nope/missing.txt")).toBe("");
    expect(filterFilePatch(twoFilePayload, "")).toBe("");
    expect(filterFilePatch("", "any.txt")).toBe("");
    // Empty input parses to zero rows (see header degeneracy suite) — never
    // a throw, never a phantom row.
    expect(parseUnifiedDiff("")).toEqual([]);
  });

  it("agrees with classifyMetaLine: meta-classified lines never leak into a kept patch body", () => {
    // For every line of every kept section, anything classifyMetaLine calls
    // meta/binary sits only in the header block before the first hunk.
    const patch = filterFilePatch(twoFilePayload, "src/a.rs");
    let seenHunk = false;
    for (const line of patch.split("\n")) {
      if (line.startsWith("@@")) seenHunk = true;
      if (seenHunk && classifyMetaLine(line)) {
        throw new Error(`meta leaked into hunk body: ${JSON.stringify(line)}`);
      }
    }
    expect(seenHunk).toBe(true);
  });
});
