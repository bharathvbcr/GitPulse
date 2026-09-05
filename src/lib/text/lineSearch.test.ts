import { describe, expect, it } from "vitest";
import {
  buildMatcher,
  escapeRegExp,
  hasUnboundedNesting,
  findMatches,
  firstMatchFrom,
  matchLabel,
  matchesInLine,
  stepMatch,
} from "./lineSearch";

describe("buildMatcher", () => {
  it("treats a plain query literally, metacharacters and all", () => {
    const matcher = buildMatcher("a.c");
    expect(matcher?.test("abc")).toBe(false);
    expect(buildMatcher("a.c")?.test("a.c")).toBe(true);
  });

  it("honours the case-sensitive flag", () => {
    expect(buildMatcher("ABC")?.test("abc")).toBe(true);
    expect(buildMatcher("ABC", { caseSensitive: true })?.test("abc")).toBe(false);
  });

  it("compiles a regex query when asked", () => {
    expect(buildMatcher("a.c", { regex: true })?.test("abc")).toBe(true);
  });

  it("returns null rather than throwing on a broken pattern", () => {
    expect(buildMatcher("([", { regex: true })).toBeNull();
  });

  it("refuses a pattern whose nesting can backtrack exponentially", () => {
    // `(a+)+c` against twenty-eight characters took 111 seconds before this
    // guard, and a JavaScript regex cannot be interrupted once it starts.
    expect(buildMatcher("(a+)+", { regex: true })).toBeNull();
    expect(hasUnboundedNesting("(a+)+")).toBe(true);
  });

  it("escapes a literal query, so nesting is impossible without regex mode", () => {
    expect(buildMatcher("(a+)+")).not.toBeNull();
  });

  it("treats an empty or blank query as no question asked", () => {
    expect(buildMatcher("")).toBeNull();
    expect(buildMatcher("   ")).toBeNull();
  });

  it("escapes every metacharacter it claims to", () => {
    const raw = ".*+?^${}()|[]\\";
    expect(new RegExp(`^${escapeRegExp(raw)}$`).test(raw)).toBe(true);
  });
});

describe("findMatches", () => {
  const lines = ["alpha beta", "beta gamma", "nothing here"];

  it("finds every occurrence, with its column and length", () => {
    expect(findMatches(lines, "beta").matches).toEqual([
      { lineIndex: 0, colStart: 6, length: 4 },
      { lineIndex: 1, colStart: 0, length: 4 },
    ]);
  });

  it("finds repeated hits within one line", () => {
    expect(findMatches(["aaa"], "a").matches).toHaveLength(3);
  });

  it("does not hang on a pattern that can match nothing", () => {
    // `exec` does not advance `lastIndex` past a zero-length match, so the
    // naive loop spins forever. Typing `*` into a search box must not hang.
    for (const pattern of ["a*", "\\b", "(?:)", "^", "$", "x?"]) {
      const started = Date.now();
      const result = findMatches(["hello world", "second line"], pattern, { regex: true });
      expect(Date.now() - started, pattern).toBeLessThan(200);
      expect(result.invalid, pattern).toBe(false);
    }
  });

  it("keeps the non-empty hits of a pattern that can also match nothing", () => {
    expect(findMatches(["abc"], "b*", { regex: true }).matches).toEqual([
      { lineIndex: 0, colStart: 1, length: 1 },
    ]);
  });

  it("stops at the cap and says so instead of reporting a floor as a total", () => {
    const result = findMatches([Array.from({ length: 500 }, () => "x").join("")], "x", {
      maxMatches: 50,
    });
    expect(result.matches).toHaveLength(50);
    expect(result.truncated).toBe(true);
  });

  it("stops scanning further lines once the cap is reached", () => {
    const many = Array.from({ length: 10_000 }, () => "match");
    const started = Date.now();
    const result = findMatches(many, "match", { maxMatches: 10 });
    expect(Date.now() - started).toBeLessThan(200);
    expect(result.matches).toHaveLength(10);
    expect(result.truncated).toBe(true);
  });

  it("reports an unusable regex rather than an empty result", () => {
    const result = findMatches(lines, "([", { regex: true });
    expect(result.invalid).toBe(true);
    expect(result.reason).toBe("syntax");
    expect(result.matches).toEqual([]);
  });

  it("says WHY a pattern was refused, so the two cases read differently", () => {
    expect(findMatches(lines, "([a-z]+\\s*)+", { regex: true }).reason).toBe("unbounded");
    expect(findMatches(lines, "([", { regex: true }).reason).toBe("syntax");
  });

  it("stops at a wall-clock deadline and marks the result truncated", () => {
    const many = { length: 3_000_000, at: (i: number) => `line ${i} with words` };
    const started = Date.now();
    const result = findMatches(many, "words", { maxMatches: 10_000_000, maxMillis: 5 });
    expect(Date.now() - started).toBeLessThan(5_000);
    expect(result.truncated).toBe(true);
    expect(result.matches.length).toBeLessThan(3_000_000);
  });

  it("is not invalid merely because the query is empty", () => {
    expect(findMatches(lines, "", { regex: true }).invalid).toBe(false);
  });

  it("reads through an accessor so a caller can search a projection", () => {
    // The diff searches line content with the +/- marker stripped, without
    // materialising a second copy of a 300,000-line array.
    const raw = ["+added text", "-removed text", " context"];
    const result = findMatches(
      { length: raw.length, at: (i: number) => raw[i].slice(1) },
      "added",
    );
    expect(result.matches).toEqual([{ lineIndex: 0, colStart: 0, length: 5 }]);
  });

  it("scans a hundred thousand lines within a frame budget", () => {
    const lots = Array.from({ length: 100_000 }, (_, i) => `line ${i} of text`);
    const started = Date.now();
    const result = findMatches(lots, "of text", { maxMatches: 200_000 });
    expect(Date.now() - started).toBeLessThan(2_000);
    expect(result.matches).toHaveLength(100_000);
  });

  it("returns no matches for an empty input", () => {
    expect(findMatches([], "a")).toEqual({ matches: [], truncated: false, invalid: false });
  });

  it("treats a zero cap as collecting nothing, not as unlimited", () => {
    expect(findMatches(["aaa"], "a", { maxMatches: 0 }).matches).toEqual([]);
  });
});

describe("matchesInLine", () => {
  it("resets the shared matcher so it does not skip the head of a later line", () => {
    const matcher = buildMatcher("a")!;
    const out: { lineIndex: number; colStart: number; length: number }[] = [];
    matchesInLine("aaa", matcher, 0, out, 10);
    matchesInLine("a", matcher, 1, out, 10);
    expect(out.filter((match) => match.lineIndex === 1)).toHaveLength(1);
  });

  it("appends nothing once the remaining budget is spent", () => {
    const out: { lineIndex: number; colStart: number; length: number }[] = [];
    expect(matchesInLine("aaa", buildMatcher("a")!, 0, out, 0)).toBe(0);
    expect(out).toEqual([]);
  });

  it("honours a partial budget", () => {
    const out: { lineIndex: number; colStart: number; length: number }[] = [];
    expect(matchesInLine("aaaa", buildMatcher("a")!, 0, out, 2)).toBe(2);
  });
});

describe("match navigation", () => {
  const matches = [
    { lineIndex: 2, colStart: 0, length: 1 },
    { lineIndex: 9, colStart: 0, length: 1 },
    { lineIndex: 40, colStart: 0, length: 1 },
  ];

  it("resumes from where the reader is looking", () => {
    expect(firstMatchFrom(matches, 0)).toBe(0);
    expect(firstMatchFrom(matches, 3)).toBe(1);
    expect(firstMatchFrom(matches, 41)).toBe(0);
  });

  it("answers -1 when there is nothing to resume to", () => {
    expect(firstMatchFrom([], 0)).toBe(-1);
  });

  it("wraps around in both directions", () => {
    expect(stepMatch(2, 3, 1)).toBe(0);
    expect(stepMatch(0, 3, -1)).toBe(2);
    expect(stepMatch(0, 3, 1)).toBe(1);
  });

  it("answers -1 for stepping through nothing", () => {
    expect(stepMatch(0, 0, 1)).toBe(-1);
  });
});

describe("matchLabel", () => {
  it("counts the current hit against the total", () => {
    expect(matchLabel({ matches: [{ lineIndex: 0, colStart: 0, length: 1 }], truncated: false, invalid: false }, 0))
      .toBe("1 of 1");
  });

  it("marks a capped count as a floor rather than a total", () => {
    const matches = Array.from({ length: 5 }, () => ({ lineIndex: 0, colStart: 0, length: 1 }));
    expect(matchLabel({ matches, truncated: true, invalid: false }, 1)).toBe("2 of 5+");
  });

  it("says a pattern is bad rather than reporting zero matches for it", () => {
    expect(matchLabel({ matches: [], truncated: false, invalid: true }, 0)).toBe("bad pattern");
  });

  it("distinguishes a refused pattern from a malformed one", () => {
    expect(
      matchLabel({ matches: [], truncated: false, invalid: true, reason: "unbounded" }, 0),
    ).toBe("pattern may not terminate");
  });

  it("says no matches when there are none", () => {
    expect(matchLabel({ matches: [], truncated: false, invalid: false }, 0)).toBe("no matches");
  });
});
