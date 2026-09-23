import { describe, expect, it } from "vitest";
import {
  groupHunksBySymbol,
  hunkSymbolLabels,
  symbolForHunk,
  type HunkLineRange,
  type SymbolSpan,
} from "./symbolGroups";

const symbols: SymbolSpan[] = [
  { symbol_name: "outer", span_start_line: 1, span_end_line: 40 },
  { symbol_name: "inner", span_start_line: 10, span_end_line: 20 },
  { symbol_name: "other", span_start_line: 50, span_end_line: 60 },
];

function hunk(
  key: string,
  old_start: number,
  old_lines: number,
  new_start = old_start,
  new_lines = old_lines,
): HunkLineRange {
  return { key, old_start, old_lines, new_start, new_lines };
}

describe("symbolForHunk", () => {
  it("prefers the tightest overlapping symbol", () => {
    expect(symbolForHunk(hunk("a", 12, 2), symbols)).toBe("inner");
  });

  it("falls back to the outer symbol outside the nested span", () => {
    expect(symbolForHunk(hunk("a", 3, 2), symbols)).toBe("outer");
  });

  it("returns empty when no symbol overlaps", () => {
    expect(symbolForHunk(hunk("a", 100, 2), symbols)).toBe("");
  });

  it("ignores pure-addition hunks with no old side when new side also misses", () => {
    expect(symbolForHunk(hunk("a", 0, 0, 100, 2), symbols)).toBe("");
  });

  it("joins on the new side when the old side is empty", () => {
    expect(symbolForHunk(hunk("a", 0, 0, 12, 1), symbols)).toBe("inner");
  });
});

describe("groupHunksBySymbol", () => {
  it("merges adjacent hunks under the same symbol", () => {
    const groups = groupHunksBySymbol(
      [hunk("1", 11, 1), hunk("2", 15, 1), hunk("3", 55, 1)],
      symbols,
    );
    expect(groups).toEqual([
      { symbol_name: "inner", hunk_keys: ["1", "2"] },
      { symbol_name: "other", hunk_keys: ["3"] },
    ]);
  });

  it("does not merge empty-symbol hunks together", () => {
    const groups = groupHunksBySymbol(
      [hunk("1", 100, 1), hunk("2", 200, 1)],
      symbols,
    );
    expect(groups).toEqual([
      { symbol_name: "", hunk_keys: ["1"] },
      { symbol_name: "", hunk_keys: ["2"] },
    ]);
  });
});

describe("hunkSymbolLabels", () => {
  it("maps every hunk key", () => {
    const labels = hunkSymbolLabels([hunk("1", 11, 1), hunk("2", 100, 1)], symbols);
    expect(labels.get("1")).toBe("inner");
    expect(labels.get("2")).toBe("");
  });
});
