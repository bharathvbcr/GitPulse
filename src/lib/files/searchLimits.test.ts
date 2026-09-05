import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { matchCountLabel, SEARCH_DEBOUNCE_MS } from "./searchLimits";

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
    const scan = viewer.slice(viewer.indexOf("let searchMatches ="));
    const body = scan.slice(0, scan.indexOf("let matchCount"));
    expect(body).toContain("debouncedQuery.trim()");
    expect(body).not.toContain("searchQuery.trim()");
  });

  it("clears immediately when the box is emptied", () => {
    // Waiting to REMOVE highlighting reads as lag and buys nothing.
    expect(viewer).toContain("applySearchQuery.cancel()");
  });

  it("stops collecting at the ceiling", () => {
    expect(viewer).toContain("const MAX_SEARCH_MATCHES = 5_000");
    expect(viewer).toContain("if (matches.length >= MAX_SEARCH_MATCHES) return matches");
  });

  it("cannot spin on a zero-width match", () => {
    // `^`, an empty alternation or a lookahead leaves lastIndex unmoved, and
    // the loop that collects global matches would never terminate.
    expect(viewer).toContain("if (match[0].length === 0) matcher.lastIndex += 1");
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
