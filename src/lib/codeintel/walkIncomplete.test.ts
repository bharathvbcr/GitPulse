import { describe, expect, it } from "vitest";
import {
  boundText,
  boundedJoin,
  summarizeWalkIncomplete,
  tooltipWalkIncomplete,
  WALK_INCOMPLETE_MAX_CHARS,
  WALK_INCOMPLETE_TOOLTIP_CHARS,
} from "./walkIncomplete";

const COVERAGE =
  "28637 of 69195 unresolved attribution site(s) have no indexed target after excluding 40548 known builtin, runtime-global, and external-import site(s); these repository-wide counts are not specific to this target, so this answer may omit callers or dependencies";

function seedWalk(unrecorded: number): string {
  return `the walk did not complete: stopped at depth 10, ${unrecorded} traversed edges unrecorded; the result is a lower bound, not the full blast radius; ${COVERAGE}`;
}

describe("summarizeWalkIncomplete", () => {
  it("returns null for empty, blank, and missing parts", () => {
    expect(summarizeWalkIncomplete([])).toBeNull();
    expect(summarizeWalkIncomplete([null, undefined, "", "   ", " · ", "; "])).toBeNull();
  });

  it("keeps a single short reason unchanged", () => {
    expect(summarizeWalkIncomplete(["unattributed calls"])).toBe("unattributed calls");
  });

  it("collapses identical copies without a seed suffix", () => {
    const copies = Array.from({ length: 179 }, () => COVERAGE);
    const text = summarizeWalkIncomplete(copies)!;
    expect(text.split("repository-wide counts").length - 1).toBe(1);
    expect(text).not.toContain("seeds");
    expect(text.length).toBeLessThan(COVERAGE.length + 10);
  });

  it("folds numeric variants into one range and keeps corpus coverage once", () => {
    const parts = Array.from({ length: 25 }, (_, i) => seedWalk(700 + i * 500));
    const text = summarizeWalkIncomplete(parts)!;
    expect(text.length).toBeLessThan(WALK_INCOMPLETE_MAX_CHARS);
    expect(text.split("repository-wide counts").length - 1).toBe(1);
    expect(text.split("the walk did not complete").length - 1).toBe(1);
    expect(text).toContain("700\u201312700");
    expect(text).toContain("25 seeds");
    expect(text).toContain("stopped at depth 10");
    expect(text).not.toContain(" · ");
  });

  it("collapses an already-joined blob from a previous compose", () => {
    const blob = Array.from({ length: 12 }, (_, i) => seedWalk(100 + i)).join(" · ");
    const text = summarizeWalkIncomplete([blob])!;
    expect(text.split("repository-wide counts").length - 1).toBe(1);
    expect(text).toContain("12 seeds");
    expect(text.length).toBeLessThan(1200);
  });

  it("keeps genuinely distinct qualifications", () => {
    const text = summarizeWalkIncomplete([
      "parse loss",
      "depth capped",
      "parse loss",
    ])!;
    expect(text).toContain("parse loss");
    expect(text).toContain("depth capped");
    expect(text.split("parse loss").length - 1).toBe(1);
  });

  it("is stable when run twice on its own output", () => {
    const first = summarizeWalkIncomplete(
      Array.from({ length: 8 }, (_, i) => seedWalk(80 + i)),
    )!;
    const second = summarizeWalkIncomplete([first]);
    expect(second).toBe(first);
  });

  it("keeps merging when a prior summary is appended with a new walk", () => {
    const prior = summarizeWalkIncomplete(
      Array.from({ length: 10 }, (_, i) => seedWalk(100 + i)),
    )!;
    const text = summarizeWalkIncomplete([prior, seedWalk(5000)])!;
    expect(text).toContain("11 seeds");
    expect(text).toContain("100\u20135000");
    expect(text.split("repository-wide counts").length - 1).toBe(1);
  });

  it("clips a single adversarial clause that exceeds the budget", () => {
    const huge = `qualification ${"x".repeat(8_000)}`;
    const text = summarizeWalkIncomplete([huge], 80)!;
    expect(text.length).toBeLessThanOrEqual(80);
    expect(text.endsWith("…")).toBe(true);
  });

  it("omits later distinct clauses when the joined budget is exhausted", () => {
    const parts = Array.from({ length: 40 }, (_, i) => {
      const a = String.fromCharCode(65 + (i % 26));
      const b = String.fromCharCode(65 + Math.floor(i / 26));
      return `unique ${a}${b} ${"z".repeat(40)}`;
    });
    const text = summarizeWalkIncomplete(parts, 200)!;
    expect(text.length).toBeLessThanOrEqual(200);
    expect(text).toMatch(/more distinct qualification\(s\) omitted/);
    expect(text).toContain("unique");
  });

  it("keeps merging when a prior similar-fold is appended with a new walk", () => {
    const first = summarizeWalkIncomplete([
      "overflow 9007199254740993 traversed",
      "overflow 9007199254740994 traversed",
    ])!;
    expect(first).toContain("2 similar");
    const text = summarizeWalkIncomplete([
      first,
      "overflow 9007199254740995 traversed",
    ])!;
    expect(text).toContain("3 similar");
    expect(text.split("overflow").length - 1).toBe(1);
  });

  it("does not leak lastIndex across sequential calls", () => {
    for (let i = 0; i < 200; i++) {
      const text = summarizeWalkIncomplete([seedWalk(i + 1), seedWalk(i + 50)])!;
      expect(text).toContain("2 seeds");
      expect(text.split("the walk did not complete").length - 1).toBe(1);
    }
  });

  it("does not treat unsafe integers as mergeable counts", () => {
    const a = "overflow 9007199254740993 traversed";
    const b = "overflow 9007199254740994 traversed";
    const text = summarizeWalkIncomplete([a, b])!;
    expect(text).toContain("2 similar");
    expect(text).not.toContain("\u2013");
  });

  it("survives mixed joiners, unicode, empty slots, and a leading separator", () => {
    const text = summarizeWalkIncomplete([
      " · a; ; b · ",
      "a",
      "café 12",
      "café 99",
      "\n\tb\n",
    ])!;
    expect(text).toContain("a");
    expect(text).toContain("b");
    expect(text).toContain("café 12\u201399");
  });

  it("folds 500 unique unrecorded counts without growing linearly", () => {
    const started = performance.now();
    const parts = Array.from({ length: 500 }, (_, i) => seedWalk(i + 1));
    const text = summarizeWalkIncomplete(parts)!;
    const elapsed = performance.now() - started;
    expect(elapsed).toBeLessThan(100);
    expect(text.length).toBeLessThan(WALK_INCOMPLETE_MAX_CHARS);
    expect(text).toContain("500 seeds");
    expect(text).toContain("1\u2013500");
    expect(text.split("repository-wide counts").length - 1).toBe(1);
  });

  it("bounds a tooltip independently of the display budget", () => {
    const text = tooltipWalkIncomplete(
      Array.from({ length: 30 }, (_, i) => seedWalk(1_000 + i)),
    )!;
    expect(text.length).toBeLessThanOrEqual(WALK_INCOMPLETE_TOOLTIP_CHARS);
    expect(text.split("repository-wide counts").length - 1).toBeLessThanOrEqual(1);
  });

  it("never exceeds a budget smaller than the omit suffix", () => {
    const parts = Array.from({ length: 20 }, (_, i) => `unique${String.fromCharCode(65 + i)}`);
    const text = summarizeWalkIncomplete(parts, 25)!;
    expect(text.length).toBeLessThanOrEqual(25);
  });

  it("keeps a prior seed-weight when merging a matching canonical clause", () => {
    const text = summarizeWalkIncomplete(["depth capped", "depth capped (10 seeds)"])!;
    expect(text).toContain("10 seeds");
    expect(text.split("depth capped").length - 1).toBe(1);
  });

  it("does not double-count a summary concatenated with itself", () => {
    const first = summarizeWalkIncomplete(
      Array.from({ length: 8 }, (_, i) => seedWalk(80 + i)),
    )!;
    const twice = summarizeWalkIncomplete([first, first])!;
    expect(twice).toBe(first);
    expect(twice).toContain("8 seeds");
    expect(twice).not.toContain("16 seeds");
  });
});

describe("boundText", () => {
  it("clips assembled titles independently of walk folding", () => {
    const text = boundText("prefix " + "x".repeat(2_000), 40);
    expect(text.length).toBeLessThanOrEqual(40);
    expect(text.startsWith("prefix")).toBe(true);
    expect(text.endsWith("…")).toBe(true);
  });

  it("clips emoji on a code-point boundary rather than a surrogate", () => {
    const text = boundText(`😀${"x".repeat(20)}`, 3);
    expect(Array.from(text).length).toBeLessThanOrEqual(3);
    expect(text.startsWith("😀")).toBe(true);
    expect(text.endsWith("…")).toBe(true);
    expect(() => JSON.stringify(text)).not.toThrow();
  });
});

describe("boundedJoin", () => {
  it("returns empty for empty input", () => {
    expect(boundedJoin([])).toBe("");
    expect(boundedJoin(["", "  "])).toBe("");
  });

  it("names the real total when the sample is capped", () => {
    const items = Array.from({ length: 179 }, (_, i) => `file-${i}.md`);
    const text = boundedJoin(items, 4, 480);
    expect(text).toContain("file-0.md");
    expect(text).toContain("179 total");
    expect(text.split("file-").length - 1).toBeLessThanOrEqual(8);
    expect(text.length).toBeLessThanOrEqual(480);
  });

  it("keeps the real total at the front so a tight clip cannot drop it", () => {
    const items = Array.from({ length: 179 }, (_, i) => `file-${i}.md`);
    const text = boundedJoin(items, 4, 40);
    expect(text.length).toBeLessThanOrEqual(40);
    expect(text).toContain("179 total");
  });

  it("clips an oversized single path", () => {
    const text = boundedJoin(["x".repeat(2_000)], 8, 40);
    expect(text.length).toBeLessThanOrEqual(40);
    expect(text.endsWith("…")).toBe(true);
  });
});
