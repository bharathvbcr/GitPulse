import { describe, expect, it } from "vitest";
import { describeSearchReport, groupByFile } from "./contentSearch";
import type { ContentMatch, ContentSearchReport } from "./types";

const m = (path: string, line: number): ContentMatch => ({ path, line, column: 1, text: "x", text_clipped: false });
const report = (over: Partial<ContentSearchReport>): ContentSearchReport => ({
  matches: [m("a.ts", 1), m("a.ts", 4), m("b.ts", 2)],
  files: 2,
  truncated: false,
  truncated_reason: null,
  revision: null,
  ...over,
});

describe("groupByFile", () => {
  it("keeps git's file order and each file's line order", () => {
    const groups = groupByFile([m("b.ts", 1), m("a.ts", 3), m("b.ts", 9)]);
    expect(groups.map((g) => g.path)).toEqual(["b.ts", "a.ts"]);
    expect(groups[0].matches.map((x) => x.line)).toEqual([1, 9]);
  });
});

describe("describeSearchReport", () => {
  it("states a complete answer without hedging", () => {
    expect(describeSearchReport(report({}))).toBe("3 matches in 2 files in the working tree");
  });

  it("never lets a partial answer read as complete", () => {
    for (const reason of ["match_limit", "output_cap", "deadline", "cancelled"] as const) {
      const text = describeSearchReport(report({ truncated: true, truncated_reason: reason }));
      expect(text).toContain("partial");
      expect(text).toContain("more matches may exist");
    }
    expect(describeSearchReport(report({ truncated: true, truncated_reason: "cancelled" }))).toContain("cancelled");
    // A reason this build does not know is still a partial answer.
    expect(describeSearchReport(report({ truncated: true, truncated_reason: "new_reason" }))).toContain(
      "partial: the search stopped early",
    );
  });

  it("names the revision searched", () => {
    expect(describeSearchReport(report({ revision: "d".repeat(40) }))).toContain("at ddddddd");
  });
});
