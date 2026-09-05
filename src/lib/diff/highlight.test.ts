import { describe, expect, it } from "vitest";
import {
  composeSpans,
  MAX_HIGHLIGHT_CHARS,
  normalizeRanges,
  segmentRanges,
  shiftMatches,
} from "./highlight";
import { computeWordDiff, type DiffSegment } from "./wordDiff";

const text = (spans: ReturnType<typeof composeSpans>) => spans.map((span) => span.text).join("");

describe("segmentRanges", () => {
  it("maps the changed side of a word diff to character ranges", () => {
    const diff = computeWordDiff("const a = 1;", "const b = 1;");
    const ranges = segmentRanges("const b = 1;", diff.modified_segments, "Added");
    expect(ranges).toEqual([{ start: 6, end: 7 }]);
  });

  it("maps the removed side against the old text", () => {
    const diff = computeWordDiff("const a = 1;", "const b = 1;");
    expect(segmentRanges("const a = 1;", diff.original_segments, "Removed")).toEqual([
      { start: 6, end: 7 },
    ]);
  });

  it("refuses segments that do not reconstruct the line", () => {
    // Painting a "changed" background over offsets belonging to other
    // characters is a confidently wrong highlight; none is better.
    const wrong: DiffSegment[] = [{ kind: "Added", text: "mismatch" }];
    expect(segmentRanges("actual text here", wrong, "Added")).toEqual([]);
  });

  it("is empty for a line with no segments at all", () => {
    expect(segmentRanges("plain", undefined, "Added")).toEqual([]);
    expect(segmentRanges("plain", [], "Added")).toEqual([]);
  });

  it("skips zero-length segments rather than emitting empty ranges", () => {
    expect(
      segmentRanges("ab", [{ kind: "Added", text: "" }, { kind: "Added", text: "ab" }], "Added"),
    ).toEqual([{ start: 0, end: 2 }]);
  });
});

describe("normalizeRanges", () => {
  it("merges overlapping and touching ranges", () => {
    expect(normalizeRanges([{ start: 5, end: 9 }, { start: 0, end: 5 }, { start: 7, end: 12 }]))
      .toEqual([{ start: 0, end: 12 }]);
  });

  it("drops empty and inverted ranges", () => {
    expect(normalizeRanges([{ start: 3, end: 3 }, { start: 8, end: 2 }])).toEqual([]);
  });

  it("drops non-finite ranges instead of producing NaN boundaries", () => {
    expect(normalizeRanges([{ start: Number.NaN, end: 4 }, { start: 0, end: Infinity }])).toEqual([]);
  });

  it("keeps disjoint ranges apart", () => {
    expect(normalizeRanges([{ start: 0, end: 2 }, { start: 5, end: 7 }])).toEqual([
      { start: 0, end: 2 },
      { start: 5, end: 7 },
    ]);
  });
});

describe("composeSpans", () => {
  it("reproduces the line exactly, whatever the layers say", () => {
    const line = 'const greeting = "hello, world"; // note';
    const diff = computeWordDiff('const greeting = "hi"; // note', line);
    const spans = composeSpans(line, "typescript", diff.modified_segments, "Added", [
      { start: 6, end: 14 },
    ]);
    expect(text(spans)).toBe(line);
  });

  it("keeps syntax, change and match answers on the same span", () => {
    const spans = composeSpans("let x = 1;", "typescript", undefined, "Added", [
      { start: 0, end: 3 },
    ]);
    const keyword = spans.find((span) => span.text.startsWith("let"));
    expect(keyword?.token).toBe("keyword");
    expect(keyword?.match).toBe(true);
  });

  it("marks only the characters the commit changed", () => {
    const before = "const a = 1;";
    const after = "const b = 1;";
    const diff = computeWordDiff(before, after);
    const spans = composeSpans(after, "typescript", diff.modified_segments, "Added");
    const changed = spans.filter((span) => span.changed).map((span) => span.text);
    expect(changed).toEqual(["b"]);
  });

  it("splits one token where a change starts inside it", () => {
    const diff = computeWordDiff("callOldName()", "callNewName()");
    const spans = composeSpans("callNewName()", "typescript", diff.modified_segments, "Added");
    expect(text(spans)).toBe("callNewName()");
    expect(spans.some((span) => span.changed)).toBe(true);
    expect(spans.some((span) => !span.changed)).toBe(true);
  });

  it("merges neighbours that agree on all three answers", () => {
    const spans = composeSpans("aaaa", "plaintext", undefined, "Added");
    expect(spans).toHaveLength(1);
  });

  it("skips tokenizing a line too long to read", () => {
    const long = `const x = "${"a".repeat(MAX_HIGHLIGHT_CHARS)}";`;
    const spans = composeSpans(long, "typescript", undefined, "Added");
    expect(text(spans)).toBe(long);
    expect(spans.every((span) => span.token === "text")).toBe(true);
  });

  it("still marks changes and matches on a line too long to tokenize", () => {
    const long = "b".repeat(MAX_HIGHLIGHT_CHARS + 10);
    const spans = composeSpans(long, "typescript", undefined, "Added", [{ start: 0, end: 5 }]);
    expect(spans[0].match).toBe(true);
    expect(spans[0].text).toHaveLength(5);
    expect(text(spans)).toBe(long);
  });

  it("honours an explicit request for no syntax highlighting", () => {
    const spans = composeSpans("let x = 1;", "typescript", undefined, "Added", [], {
      syntax: false,
    });
    expect(spans.every((span) => span.token === "text")).toBe(true);
  });

  it("returns nothing for an empty line", () => {
    expect(composeSpans("", "typescript", undefined, "Added")).toEqual([]);
  });

  it("reproduces the line for every language it claims to support", () => {
    const samples: Array<[string, string]> = [
      ["typescript", "export const x: number = 1; // c"],
      ["rust", 'fn main() { let s = "hi"; }'],
      ["python", "def f(x):  # comment\n"],
      ["go", 'func main() { fmt.Println("hi") }'],
      ["json", '{"a": [1, 2, null]}'],
      ["yaml", "key: value # note"],
      ["markdown", "# Title *bold* `code`"],
      ["css", ".cls { color: #fff; }"],
      ["html", '<div class="a">text</div>'],
      ["shell", "echo \"$HOME\" | grep -q x"],
      ["sql", "SELECT * FROM t WHERE a = 1;"],
      ["toml", "[section]\nkey = 1"],
      ["svelte", "{#if ok}<p>{value}</p>{/if}"],
      ["xml", "<a b='c'>d</a>"],
      ["c", "int main(void) { return 0; }"],
      ["cpp", "auto x = std::vector<int>{1};"],
      ["javascript", "const re = /ab+c/g;"],
      ["diff", "+added line"],
      ["plaintext", "just words"],
    ];
    for (const [language, line] of samples) {
      const spans = composeSpans(line, language as never, undefined, "Added");
      expect(text(spans), language).toBe(line);
    }
  });

  it("reproduces hostile lines exactly", () => {
    const hostile = [
      "\t\tindented\twith\ttabs",
      "unterminated \"string",
      "emoji 🚀 and é and 中文",
      "'''",
      "/* unclosed comment",
      "\\",
      "   ",
    ];
    for (const line of hostile) {
      expect(text(composeSpans(line, "typescript", undefined, "Added")), line).toBe(line);
      expect(text(composeSpans(line, "rust", undefined, "Removed")), line).toBe(line);
    }
  });

  it("composes syntax, a real word diff and a search hit on one line", () => {
    const before = "  const total = countOf(items);";
    const after = "  const total = countOf(rows) + 1;";
    const diff = computeWordDiff(before, after);
    const spans = composeSpans(after, "typescript", diff.modified_segments, "Added", [
      { start: 8, end: 13 },
    ]);
    expect(text(spans)).toBe(after);
    expect(spans.some((span) => span.changed && span.match)).toBe(false);
    expect(spans.some((span) => span.match)).toBe(true);
    expect(spans.some((span) => span.changed)).toBe(true);
  });
});

describe("shiftMatches", () => {
  it("moves a hit from the raw line to the rendered text", () => {
    // A diff row draws `content.slice(1)`; a hit at raw column 4 is at
    // rendered column 3, and getting this wrong shifts every highlight by one.
    expect(shiftMatches([{ colStart: 4, length: 3 }], 1, 20)).toEqual([{ start: 3, end: 6 }]);
  });

  it("clips a hit that starts inside the marker column", () => {
    expect(shiftMatches([{ colStart: 0, length: 3 }], 1, 20)).toEqual([{ start: 0, end: 2 }]);
  });

  it("drops a hit that lies entirely in the marker column", () => {
    expect(shiftMatches([{ colStart: 0, length: 1 }], 1, 20)).toEqual([]);
  });

  it("clips a hit running past the end of the text", () => {
    expect(shiftMatches([{ colStart: 1, length: 99 }], 1, 5)).toEqual([{ start: 0, end: 5 }]);
  });

  it("drops a hit beyond the text entirely", () => {
    expect(shiftMatches([{ colStart: 40, length: 2 }], 1, 5)).toEqual([]);
  });

  it("passes hits through untouched when there is no marker", () => {
    expect(shiftMatches([{ colStart: 2, length: 3 }], 0, 20)).toEqual([{ start: 2, end: 5 }]);
  });
});
