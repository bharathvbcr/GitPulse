import { describe, expect, it } from "vitest";
import {
  CODE_ZOOM_MAX,
  CODE_ZOOM_MIN,
  rowHeight,
  scaledRowHeight,
} from "./density";

/**
 * The code viewer used to size the virtual slot from zoom and paint the glyph
 * from a fixed 20px leading. These attacks are the cases that made two rows
 * occupy the same pixels.
 */
describe("scaledRowHeight stress", () => {
  const bases = [rowHeight("code", "spacious"), rowHeight("code", "compact")];
  const zooms = [CODE_ZOOM_MIN, 80, 90, 100, 110, 130, 150, CODE_ZOOM_MAX];

  it("keeps the unzoomed slot equal to the density height", () => {
    for (const base of bases) {
      expect(scaledRowHeight(base, 100)).toBe(base);
    }
  });

  it("never returns the spacious leading for a compact slot", () => {
    const compact = rowHeight("code", "compact");
    const spacious = rowHeight("code", "spacious");
    expect(scaledRowHeight(compact, 100)).toBe(compact);
    expect(scaledRowHeight(compact, 100)).toBeLessThan(spacious);
  });

  it.each(bases)("stays a positive integer at every control stop for base %s", (base) => {
    for (const zoom of zooms) {
      const height = scaledRowHeight(base, zoom);
      expect(Number.isInteger(height)).toBe(true);
      expect(height).toBeGreaterThan(0);
      expect(height).toBe(Math.round(base * (zoom / 100)));
    }
  });

  it("clamps zoom outside the control range instead of inventing a slot", () => {
    const base = 20;
    expect(scaledRowHeight(base, 1)).toBe(scaledRowHeight(base, CODE_ZOOM_MIN));
    expect(scaledRowHeight(base, 0)).toBe(scaledRowHeight(base, CODE_ZOOM_MIN));
    expect(scaledRowHeight(base, -40)).toBe(scaledRowHeight(base, CODE_ZOOM_MIN));
    expect(scaledRowHeight(base, 10_000)).toBe(scaledRowHeight(base, CODE_ZOOM_MAX));
    expect(scaledRowHeight(base, CODE_ZOOM_MAX + 1)).toBe(scaledRowHeight(base, CODE_ZOOM_MAX));
  });

  it("fails closed on non-finite zoom and base", () => {
    expect(scaledRowHeight(20, Number.NaN)).toBe(20);
    expect(scaledRowHeight(20, Number.POSITIVE_INFINITY)).toBe(20);
    expect(scaledRowHeight(20, Number.NEGATIVE_INFINITY)).toBe(20);
    for (const bad of [Number.NaN, 0, -1, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
      expect(scaledRowHeight(bad, 100)).toBe(0);
    }
  });

  it("grows monotonically inside the control range", () => {
    for (const base of bases) {
      let previous = 0;
      for (const zoom of zooms) {
        const height = scaledRowHeight(base, zoom);
        expect(height).toBeGreaterThanOrEqual(previous);
        previous = height;
      }
    }
  });
});
