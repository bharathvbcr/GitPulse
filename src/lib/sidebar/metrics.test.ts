import { describe, expect, it } from "vitest";
import {
  BRANCH_OVERSCAN,
  BRANCH_ROW_HEIGHT,
  SIDEBAR_COLLAPSED_WIDTH,
  SIDEBAR_DEFAULT_WIDTH,
  SIDEBAR_MAX_WIDTH,
  SIDEBAR_MIN_WIDTH,
  SIDEBAR_RESIZE_STEP,
  branchRowHeight,
  clampSidebarWidth,
} from "./metrics";

describe("branchRowHeight", () => {
  it("returns a distinct height per density and defaults to spacious", () => {
    expect(branchRowHeight("spacious")).toBe(BRANCH_ROW_HEIGHT.spacious);
    expect(branchRowHeight("compact")).toBe(BRANCH_ROW_HEIGHT.compact);
    expect(BRANCH_ROW_HEIGHT.spacious).toBeGreaterThan(BRANCH_ROW_HEIGHT.compact);
  });

  it("falls back to the spacious height for unknown density strings", () => {
    // Density arrives from a persisted store; a corrupted value must not
    // yield an undefined row height that breaks window math.
    expect(branchRowHeight("garbage" as "spacious")).toBe(BRANCH_ROW_HEIGHT.spacious);
    expect(branchRowHeight(undefined as unknown as "spacious")).toBe(
      BRANCH_ROW_HEIGHT.spacious,
    );
  });
});

describe("clampSidebarWidth", () => {
  it("keeps in-range widths intact", () => {
    for (const w of [SIDEBAR_MIN_WIDTH, SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MAX_WIDTH]) {
      expect(clampSidebarWidth(w)).toBe(w);
    }
  });

  it("clamps below the minimum and above the maximum", () => {
    expect(clampSidebarWidth(0)).toBe(SIDEBAR_MIN_WIDTH);
    expect(clampSidebarWidth(SIDEBAR_MIN_WIDTH - 1)).toBe(SIDEBAR_MIN_WIDTH);
    expect(clampSidebarWidth(100_000)).toBe(SIDEBAR_MAX_WIDTH);
    expect(clampSidebarWidth(SIDEBAR_MAX_WIDTH + 1)).toBe(SIDEBAR_MAX_WIDTH);
  });

  it("fail-closes hostile inputs to the default width", () => {
    expect(clampSidebarWidth(Number.NaN)).toBe(SIDEBAR_DEFAULT_WIDTH);
    expect(clampSidebarWidth(Number.POSITIVE_INFINITY)).toBe(SIDEBAR_DEFAULT_WIDTH);
    expect(clampSidebarWidth(Number.NEGATIVE_INFINITY)).toBe(SIDEBAR_DEFAULT_WIDTH);
  });

  it("rounds fractional drag output to whole pixels", () => {
    expect(clampSidebarWidth(SIDEBAR_DEFAULT_WIDTH + 0.4)).toBe(SIDEBAR_DEFAULT_WIDTH);
    expect(clampSidebarWidth(SIDEBAR_DEFAULT_WIDTH + 0.6)).toBe(SIDEBAR_DEFAULT_WIDTH + 1);
  });
});

describe("invariants", () => {
  it("resize step stays positive and collapsed rail is narrower than any open width", () => {
    expect(SIDEBAR_RESIZE_STEP).toBeGreaterThan(0);
    expect(BRANCH_OVERSCAN).toBeGreaterThanOrEqual(0);
    expect(SIDEBAR_COLLAPSED_WIDTH).toBeLessThan(SIDEBAR_MIN_WIDTH);
    expect(SIDEBAR_MIN_WIDTH).toBeLessThan(SIDEBAR_DEFAULT_WIDTH);
    expect(SIDEBAR_DEFAULT_WIDTH).toBeLessThan(SIDEBAR_MAX_WIDTH);
  });
});
