import { describe, expect, it } from "vitest";
import { COVERAGE_FACTOR, COVERAGE_FLOOR_MS, isCoverageRun, wallClockBudget } from "./wallClock";

describe("wallClockBudget", () => {
  it("is the number the test wrote on an ordinary run", () => {
    expect(wallClockBudget(120, {})).toBe(120);
    expect(wallClockBudget(120, { GITPULSE_COVERAGE_RUN: "" })).toBe(120);
    expect(wallClockBudget(120, { GITPULSE_COVERAGE_RUN: "0" })).toBe(120);
  });

  it("is multiplied under coverage, and never below the floor", () => {
    const coverage = { GITPULSE_COVERAGE_RUN: "1" };
    expect(wallClockBudget(1_000, coverage)).toBe(1_000 * COVERAGE_FACTOR);
    expect(wallClockBudget(10, coverage)).toBe(COVERAGE_FLOOR_MS);
    expect(wallClockBudget(0, coverage)).toBe(COVERAGE_FLOOR_MS);
  });

  it("refuses a limit that is not a usable number instead of passing everything", () => {
    for (const bad of [Number.NaN, -1, Number.POSITIVE_INFINITY]) {
      expect(() => wallClockBudget(bad, {})).toThrow(RangeError);
    }
  });

  it("reads the process environment by default, which vitest.config.ts sets for coverage runs", () => {
    expect(isCoverageRun()).toBe(isCoverageRun(process.env));
    expect(wallClockBudget(50)).toBeGreaterThanOrEqual(50);
  });
});
