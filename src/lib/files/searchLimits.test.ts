import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { matchCountLabel, SEARCH_DEBOUNCE_MS } from "./searchLimits";
import { DEFAULT_MAX_MATCHES, findMatches } from "../text/lineSearch";

const viewer = readFileSync(
  new URL("../components/files/CodeViewer.svelte", import.meta.url),
  "utf8",
);

describe("matchCountLabel", () => {
  it("reports an exact count when the scan finished", () => {
    expect(matchCountLabel(12, 5000, 0)).toBe("1 of 12");
  });

  it("marks a capped count as a floor, never as the whole truth", () => {
    // A scan that stopped at its ceiling has not counted the matches; saying
    // "5000" would present a bounded sample as complete coverage.
    expect(matchCountLabel(5000, 5000, 3)).toBe("4 of 5000+");
  });

  it("says nothing found rather than 1 of 0", () => {
    expect(matchCountLabel(0, 5000, 0)).toBe("0 matches");
  });
});

describe("in-file search is bounded and debounced", () => {
  it("scans a debounced copy of the query, not the bound input", () => {
    // `bind:value={searchQuery}` re-ran the whole-file scan on every keystroke.
    expect(viewer).toContain("let debouncedQuery = $state(\"\")");
    expect(viewer).toContain("const applySearchQuery = debounce(");
    const scan = viewer.slice(viewer.indexOf("let searchResult ="));
    const body = scan.slice(0, scan.indexOf("let matchCount"));
    expect(body).toContain("debouncedQuery");
    expect(body).not.toContain("findMatches(rawLines, searchQuery");
  });

  it("clears immediately when the box is emptied", () => {
    // Waiting to REMOVE highlighting reads as lag and buys nothing.
    expect(viewer).toContain("applySearchQuery.cancel()");
  });

  /*
   * The cap and the zero-width guard were asserted here as literal lines of an
   * inline loop. That loop is gone: the diff viewer's find bar needed the same
   * behaviour, so it lives in `text/lineSearch` and both readers call it. What
   * this file still has to prove is that the viewer DELEGATES rather than
   * growing a second, unbounded loop of its own — the bounds themselves are
   * covered behaviourally in `text/lineSearch.test.ts`, which also holds the
   * deadline and the catastrophic-backtracking refusal this never had.
   */
  it("stops collecting at the ceiling", () => {
    expect(viewer).toContain("findMatches(");
    expect(DEFAULT_MAX_MATCHES).toBe(5_000);
    // No second matcher: a hand-rolled RegExp here would be outside the bound.
    expect(viewer).not.toContain("new RegExp(");
  });

  it("cannot spin on a zero-width match", () => {
    // `^`, an empty alternation or a lookahead leaves lastIndex unmoved, and a
    // loop collecting global matches would never terminate. Proven against the
    // owner, so it holds for the diff viewer's find bar too.
    const spinning = findMatches(["aaa", "bbb"], "a*", { regex: true });
    expect(spinning.matches.length).toBeGreaterThan(0);
    expect(spinning.invalid).toBe(false);
  });

  it("uses a debounce short enough to feel immediate", () => {
    expect(SEARCH_DEBOUNCE_MS).toBeGreaterThan(0);
    expect(SEARCH_DEBOUNCE_MS).toBeLessThanOrEqual(150);
  });
});

describe("the open file is split once per change", () => {
  it("derives the truncation flag from the same split as the lines", () => {
    // Two `content.split("\\n")` calls meant a second full pass and a second
    // full allocation over a string that can be 80,000 lines long.
    const splits = viewer.match(/text\.split\("\\n"\)/g) ?? [];
    expect(splits).toHaveLength(1);
    expect(viewer).toContain("let rawLines = $derived(splitFile.lines)");
    expect(viewer).toContain("let linesTruncated = $derived(splitFile.truncated)");
  });
});
