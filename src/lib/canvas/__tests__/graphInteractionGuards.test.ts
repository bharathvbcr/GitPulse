import { describe, expect, it } from "vitest";
import { positionGraphTooltip } from "../graphInteraction";

/**
 * Hardening for the NaN-propagation hole the audit flagged: clamp(NaN) is
 * NaN, and a NaN translate3d froze the tooltip at its last position. The
 * guard must fall back to a deterministic corner-anchored placement instead.
 */
describe("positionGraphTooltip NaN/degenerate guards", () => {
  const W = 320;
  const H = 190;

  it("returns a finite placement when any coordinate is NaN", () => {
    const variants: Array<[number, number, number, number, number, number]> = [
      [Number.NaN, 10, 800, 600, W, H],
      [10, Number.NaN, 800, 600, W, H],
      [10, 10, Number.NaN, 600, W, H],
      [10, 10, 800, Number.NaN, W, H],
      [10, 10, 800, 600, Number.NaN, H],
      [10, 10, 800, 600, W, Number.NaN],
    ];
    for (const args of variants) {
      const p = positionGraphTooltip(...args);
      expect(Number.isFinite(p.left)).toBe(true);
      expect(Number.isFinite(p.top)).toBe(true);
      expect(["above", "below"]).toContain(p.placement);
    }
  });

  it("returns a finite placement for Infinity inputs too", () => {
    const p = positionGraphTooltip(Number.POSITIVE_INFINITY, 0, 800, 600, W, H);
    expect(Number.isFinite(p.left)).toBe(true);
    expect(Number.isFinite(p.top)).toBe(true);
  });

  it("degenerate tooltip sizes still produce in-bounds finite output", () => {
    const p = positionGraphTooltip(40, 40, 100, 100, 0, -50);
    expect(Number.isFinite(p.left)).toBe(true);
    expect(Number.isFinite(p.top)).toBe(true);
    expect(p.left).toBeGreaterThanOrEqual(8);
    expect(p.top).toBeGreaterThanOrEqual(8);
  });

  it("sane inputs are unchanged by the guard", () => {
    const p = positionGraphTooltip(100, 100, 800, 600, W, H);
    expect(p.placement).toBe("below");
    expect(p.left).toBe(112); // pointer + 12px gap
  });
});
