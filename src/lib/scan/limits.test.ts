import { describe, expect, it } from "vitest";
import { cappedSuffix, observedTotal } from "./limits";

describe("observedTotal", () => {
  it("uses the retained row count when no notices exist", () => {
    expect(observedTotal(null, "files", 12)).toBe(12);
    expect(observedTotal({ limit_notices: null }, "files", 7)).toBe(7);
  });

  it("falls back when notices omit the target resource", () => {
    const report = { limit_notices: [{ resource: "commits", kept: 2, total: 4 }] };
    expect(observedTotal(report, "files", 5)).toBe(5);
  });

  it("uses notice totals when they are usable and not below retained count", () => {
    const report = { limit_notices: [{ resource: "files", kept: 10, total: 42 }, { resource: "commits", kept: 2, total: 3 }] };
    expect(observedTotal(report, "files", 10)).toBe(42);
  });

  it("never lets a notice claim fewer rows than retained", () => {
    const report = { limit_notices: [{ resource: "files", kept: 50, total: 10 }] };
    expect(observedTotal(report, "files", 50)).toBe(50);
  });

  it("clamps non-finite and invalid totals to retained fallback", () => {
    const report = {
      limit_notices: [{ resource: "files", kept: 6, total: Infinity }],
    };
    expect(observedTotal(report, "files", 6)).toBe(6);
  });

  it("clamps enormous totals to JS safe integer maximum", () => {
    const report = { limit_notices: [{ resource: "files", kept: 1, total: Number.MAX_VALUE }] };
    expect(observedTotal(report, "files", 1)).toBe(Number.MAX_SAFE_INTEGER);
  });
});

describe("cappedSuffix", () => {
  it("indicates truncation when observed rows exceed shown rows", () => {
    expect(cappedSuffix(30, 10)).toBe("; showing 10");
  });

  it("returns empty text when there was no truncation", () => {
    expect(cappedSuffix(10, 30)).toBe("");
    expect(cappedSuffix(10, 10)).toBe("");
  });
});
