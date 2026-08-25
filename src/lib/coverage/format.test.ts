import { describe, it, expect } from "vitest";
import { coverageBarColor, coverageHitClass, formatCoveragePercent } from "./format";

describe("coverage format helpers", () => {
  it("maps percentages onto traffic-light colors", () => {
    expect(coverageBarColor(80)).toBe("#34d399");
    expect(coverageBarColor(50)).toBe("#fbbf24");
    expect(coverageBarColor(49.9)).toBe("#f87171");
  });

  it("classifies line hits for the blame gutter", () => {
    expect(coverageHitClass(undefined)).toBe("");
    expect(coverageHitClass(3)).toBe("bg-emerald-500/15");
    expect(coverageHitClass(0)).toBe("bg-red-500/20");
  });

  it("formats finite percentages and treats NaN as zero", () => {
    expect(formatCoveragePercent(66.66)).toBe("66.7%");
    expect(formatCoveragePercent(Number.NaN)).toBe("0.0%");
  });

  it("treats non-finite and negative percentages as zero for bar colors", () => {
    expect(coverageBarColor(Number.NaN)).toBe("#f87171");
    expect(coverageBarColor(Number.POSITIVE_INFINITY)).toBe("#f87171");
    expect(coverageBarColor(Number.NEGATIVE_INFINITY)).toBe("#f87171");
    expect(coverageBarColor(-12)).toBe("#f87171");
  });

  it("still colors finite out-of-range high percentages green", () => {
    expect(coverageBarColor(120)).toBe("#34d399");
  });

  it("classifies non-finite and negative hit counts as unknown", () => {
    expect(coverageHitClass(Number.NaN)).toBe("");
    expect(coverageHitClass(Number.POSITIVE_INFINITY)).toBe("");
    expect(coverageHitClass(Number.NEGATIVE_INFINITY)).toBe("");
    expect(coverageHitClass(-1)).toBe("");
  });

  it("never formats negative percentages", () => {
    expect(formatCoveragePercent(-5)).toBe("0.0%");
    expect(formatCoveragePercent(-0.01)).toBe("0.0%");
  });

  it("formats non-finite percentages as zero", () => {
    expect(formatCoveragePercent(Number.POSITIVE_INFINITY)).toBe("0.0%");
    expect(formatCoveragePercent(Number.NEGATIVE_INFINITY)).toBe("0.0%");
  });

  it("renders finite above-hundred percentages as-is", () => {
    expect(formatCoveragePercent(105.42)).toBe("105.4%");
  });
});
