import { describe, it, expect } from "vitest";
import {
  clamp01,
  lerp,
  easeOutCubic,
  damp,
  prefersReducedMotion,
  motionDuration,
  fadeParams,
  scaleParams,
  type MediaMatch,
} from "../easing";

function media(matches: boolean, queryOut?: { last: string }): MediaMatch {
  return {
    matchMedia: (query: string) => {
      if (queryOut) queryOut.last = query;
      return { matches } as MediaQueryList;
    },
  };
}

describe("easing", () => {
  it("clamps to the unit interval", () => {
    expect(clamp01(-2)).toBe(0);
    expect(clamp01(0.3)).toBe(0.3);
    expect(clamp01(4)).toBe(1);
  });

  it("lerps and eases out cubically", () => {
    expect(lerp(10, 20, 0.5)).toBe(15);
    expect(easeOutCubic(0)).toBe(0);
    expect(easeOutCubic(1)).toBe(1);
    expect(easeOutCubic(0.5)).toBeGreaterThan(0.5);
  });

  it("halves remaining distance after one half-life", () => {
    expect(damp(0, 1, 70, 70)).toBeCloseTo(0.5, 5);
    expect(damp(0.25, 1, 0, 70)).toBe(0.25);
    expect(damp(0, 1, 2000, 70)).toBe(1);
  });

  it("honors prefers-reduced-motion for durations and overlay params", () => {
    const q = { last: "" };
    expect(prefersReducedMotion(media(true, q))).toBe(true);
    expect(q.last).toBe("(prefers-reduced-motion: reduce)");
    expect(prefersReducedMotion(media(false))).toBe(false);
    expect(prefersReducedMotion(null)).toBe(false);

    expect(motionDuration(140, media(true))).toBe(0);
    expect(motionDuration(140, media(false))).toBe(140);
    expect(fadeParams(media(true)).duration).toBe(0);
    expect(scaleParams(media(false))).toEqual({ duration: 180, start: 0.97 });
  });
});
