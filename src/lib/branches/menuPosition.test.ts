import { describe, expect, it } from "vitest";
import { STRESS_TIMEOUT_MS } from "../__tests__/perfBudget";
import { clampMenuPosition } from "./menuPosition";

/** Deterministic PRNG so fuzz failures reproduce exactly (mulberry32). */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

describe("clampMenuPosition: fits", () => {
  it("leaves an anchor whose menu fits fully inside the viewport", () => {
    expect(clampMenuPosition(100, 80, 150, 200, 1000, 800)).toEqual({
      left: 100,
      top: 80,
    });
  });

  it("allows a menu flush against the exact right/bottom edges", () => {
    expect(clampMenuPosition(850, 600, 150, 200, 1000, 800)).toEqual({
      left: 850,
      top: 600,
    });
  });
});

describe("clampMenuPosition: overflow flips by measured size", () => {
  it("pulls a right-edge overflow back by the menu width", () => {
    expect(clampMenuPosition(900, 80, 150, 200, 1000, 800)).toEqual({
      left: 850,
      top: 80,
    });
  });

  it("pulls a bottom-edge overflow back by the menu height", () => {
    expect(clampMenuPosition(100, 700, 150, 200, 1000, 800)).toEqual({
      left: 100,
      top: 600,
    });
  });

  it("flushes to the left/top edge when the menu is larger than the viewport", () => {
    expect(clampMenuPosition(500, 500, 1200, 900, 1000, 800)).toEqual({
      left: 0,
      top: 0,
    });
  });
});

describe("clampMenuPosition: hostile inputs fail closed", () => {
  it("clamps negative anchors to the viewport origin", () => {
    expect(clampMenuPosition(-50, -50, 150, 200, 1000, 800)).toEqual({
      left: 0,
      top: 0,
    });
  });

  it("collapses NaN coordinates to the origin", () => {
    expect(clampMenuPosition(Number.NaN, Number.NaN, 150, 200, 1000, 800)).toEqual({
      left: 0,
      top: 0,
    });
  });

  it("collapses NaN menu sizes to zero-size (no correction possible)", () => {
    // Unknown size cannot be corrected for; anchor survives if on-screen.
    expect(clampMenuPosition(100, 100, Number.NaN, Number.NaN, 1000, 800)).toEqual({
      left: 100,
      top: 100,
    });
  });

  it("collapses Infinity anchors sanely", () => {
    expect(
      clampMenuPosition(
        Number.POSITIVE_INFINITY,
        Number.NEGATIVE_INFINITY,
        150,
        200,
        1000,
        800
      )
    ).toEqual({ left: 0, top: 0 });
  });

  it("treats every non-finite size uniformly as unknown-zero geometry", () => {
    // One sanitizer rule for all hostile values: non-finite size means "no
    // correction possible", so an on-screen anchor survives untouched.
    expect(
      clampMenuPosition(
        10,
        10,
        Number.POSITIVE_INFINITY,
        Number.NEGATIVE_INFINITY,
        1000,
        800
      )
    ).toEqual({ left: 10, top: 10 });
  });

  it("handles a zero-sized viewport by pinning to the origin", () => {
    expect(clampMenuPosition(400, 300, 150, 200, 0, 0)).toEqual({ left: 0, top: 0 });
  });

  it("handles negative viewport dimensions like a degenerate viewport", () => {
    expect(clampMenuPosition(400, 300, 150, 200, -100, -100)).toEqual({
      left: 0,
      top: 0,
    });
  });

  it("always returns finite numbers for extreme magnitudes", () => {
    expect(clampMenuPosition(1e308, 1e308, 1e308, 1e308, 1e9, 1e9)).toEqual({
      left: 0,
      top: 0,
    });
  });
});

describe("clampMenuPosition stress: randomized invariants", () => {
  it("holds fit/containment invariants across 20k hostile combinations", () => {
    const rand = mulberry32(0xbeef01);
    const wild = (): number => {
      const roll = rand();
      if (roll < 0.15) return Number.NaN;
      if (roll < 0.25) return Number.POSITIVE_INFINITY;
      if (roll < 0.3) return Number.NEGATIVE_INFINITY;
      if (roll < 0.45) return -(rand() * 2000);
      if (roll < 0.7) return rand() * 2000;
      if (roll < 0.85) return rand() * 200_000;
      return rand() * 1e12;
    };

    for (let i = 0; i < 20_000; i += 1) {
      const x = wild();
      const y = wild();
      const mw = wild();
      const mh = wild();
      const vw = rand() < 0.05 ? 0 : Math.floor(rand() * 1600);
      const vh = rand() < 0.05 ? 0 : Math.floor(rand() * 1000);

      const pos = clampMenuPosition(x, y, mw, mh, vw, vh);

      // Output is always finite.
      expect(Number.isFinite(pos.left)).toBe(true);
      expect(Number.isFinite(pos.top)).toBe(true);
      // Never off-screen left/top — even when the menu exceeds the viewport.
      expect(pos.left).toBeGreaterThanOrEqual(0);
      expect(pos.top).toBeGreaterThanOrEqual(0);
      expect(pos.left).toBeLessThanOrEqual(Math.max(0, vw));
      expect(pos.top).toBeLessThanOrEqual(Math.max(0, vh));
      // Either the menu fits fully, or it is flush at the edge because it
      // cannot fit (menu larger than viewport); non-finite/negative sizes
      // sanitize to zero-size, leaving a sane anchor uncorrected.
      if (Number.isFinite(mw) && mw >= 0) {
        if (mw <= vw) {
          expect(pos.left + mw).toBeLessThanOrEqual(vw + 1e-9);
        } else {
          expect(pos.left).toBe(0);
        }
      }
      if (Number.isFinite(mh) && mh >= 0) {
        if (mh <= vh) {
          expect(pos.top + mh).toBeLessThanOrEqual(vh + 1e-9);
        } else {
          expect(pos.top).toBe(0);
        }
      }
    }
  }, STRESS_TIMEOUT_MS);
});
