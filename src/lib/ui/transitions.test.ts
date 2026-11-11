import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { BAKED_SPRINGS } from "../motion/springs.generated";
import { DURATION_MS, EXIT_EASE, EXIT_MS } from "../motion/tokens";
import {
  BACKDROP_IN_MS,
  BACKDROP_OUT_MS,
  CARD_IN_MS,
  CARD_OUT_MS,
  CARD_START,
  CARD_START_MAC,
  LIQUID_SELECTION_MS,
  backdropFade,
  backdropFadeOut,
  cardScale,
  cardScaleOut,
  cubicBezierEasing,
  liquidSelection,
  springEasing,
} from "./transitions";

function media(matches: boolean) {
  return { matchMedia: () => ({ matches }) as MediaQueryList };
}

describe("modal transition params", () => {
  // Pin the standard profile: Node now exposes the host platform through
  // navigator. Mac behavior has its own coverage in macAppearance.test.ts.
  beforeEach(() => vi.stubGlobal("navigator", { platform: "Win32" }));
  afterEach(() => vi.unstubAllGlobals());

  it("takes every duration from the motion tokens; every OUT is strictly shorter than its IN twin", () => {
    // The durations are the Wave 0 tokens, not literals of this file.
    expect(BACKDROP_IN_MS).toBe(DURATION_MS.fast);
    expect(BACKDROP_OUT_MS).toBe(EXIT_MS.fast);
    expect(CARD_IN_MS).toBe(DURATION_MS.slow);
    expect(CARD_OUT_MS).toBe(EXIT_MS.slow);
    // The numbers those tokens resolve to, so a token edit shows up here.
    expect([BACKDROP_IN_MS, BACKDROP_OUT_MS, CARD_IN_MS, CARD_OUT_MS]).toEqual([140, 70, 320, 70]);
    expect(BACKDROP_OUT_MS).toBeLessThan(BACKDROP_IN_MS);
    expect(CARD_OUT_MS).toBeLessThan(CARD_IN_MS);
    expect(LIQUID_SELECTION_MS).toBe(280);
  });

  it("produces deterministic params that preserve the entrance feel", () => {
    expect(backdropFade(media(false))).toEqual({ duration: 140 });
    expect(backdropFadeOut(media(false))).toEqual({ duration: 70 });
    expect(cardScale(media(false))).toEqual({ duration: 320, start: 0.97, easing: expect.any(Function) });
    expect(cardScaleOut(media(false))).toEqual({ duration: 70, start: 0.97, easing: expect.any(Function) });
    expect(CARD_START).toBe(0.97);
    expect(CARD_START_MAC).toBe(0.985);
  });

  it("collapses to zero under prefers-reduced-motion, including OUTs", () => {
    expect(backdropFade(media(true))).toEqual({ duration: 0 });
    expect(backdropFadeOut(media(true))).toEqual({ duration: 0 });
    expect(cardScale(media(true))).toEqual({ duration: 0, start: 0.97, easing: expect.any(Function) });
    expect(cardScaleOut(media(true))).toEqual({ duration: 0, start: 0.97, easing: expect.any(Function) });
  });

  it("treats a missing media source as motion-enabled", () => {
    expect(backdropFade(null)).toEqual({ duration: BACKDROP_IN_MS });
    expect(backdropFadeOut(null)).toEqual({ duration: BACKDROP_OUT_MS });
    expect(cardScale(null)).toEqual({ duration: CARD_IN_MS, start: CARD_START, easing: expect.any(Function) });
    expect(cardScaleOut(null)).toEqual({ duration: CARD_OUT_MS, start: CARD_START, easing: expect.any(Function) });
  });

  it("enters on the spatial spring and leaves on the exit curve", () => {
    const enter = cardScale(null).easing;
    const leave = cardScaleOut(null).easing;
    expect(enter?.(0)).toBe(0);
    expect(enter?.(1)).toBe(1);
    expect(leave?.(0)).toBe(0);
    expect(leave?.(1)).toBe(1);
    // Spring: ahead of linear early on and past 1 once (the shipped overshoot).
    expect(enter?.(0.2)).toBeGreaterThan(0.5);
    expect(enter?.(0.51)).toBeGreaterThan(1);
    // Exit: an ease-in, so it starts slower than linear and never overshoots.
    expect(leave?.(0.5)).toBeLessThan(0.5);
  });

  it("keeps the liquid selection on the same spring, with the duration read when it starts", () => {
    const { easing, duration } = liquidSelection(null);
    expect(easing?.(0.2)).toBe(springEasing("spatial")(0.2));
    expect(typeof duration).toBe("function");
  });
});

describe("springEasing", () => {
  it.each(["spatialFast", "spatial", "spatialSlow", "effect"] as const)(
    "%s reproduces its baked stops exactly and interpolates between them",
    (name) => {
      const ease = springEasing(name);
      const stops = BAKED_SPRINGS[name].points;
      for (const [x, y] of stops) expect(ease(x)).toBeCloseTo(y, 12);
      const [[x0, y0], [x1, y1]] = [stops[1], stops[2]];
      expect(ease((x0 + x1) / 2)).toBeCloseTo((y0 + y1) / 2, 12);
    },
  );

  it("clamps progress outside 0..1", () => {
    const ease = springEasing("spatial");
    expect(ease(-0.5)).toBe(0);
    expect(ease(1.5)).toBe(1);
  });

  it("never overshoots on the critically damped effect curve", () => {
    const ease = springEasing("effect");
    for (let t = 0; t <= 1; t += 0.01) expect(ease(t)).toBeLessThanOrEqual(1 + 1e-9);
  });
});

describe("cubicBezierEasing", () => {
  it("is the identity for the linear bezier and pinned at the ends", () => {
    const linear = cubicBezierEasing(0, 0, 1, 1);
    for (const t of [0, 0.1, 0.5, 0.9, 1]) expect(linear(t)).toBeCloseTo(t, 4);
  });

  it("matches a known point of the exit curve (CSS cubic-bezier(0.4, 0, 1, 1))", () => {
    expect(EXIT_EASE).toBe("cubic-bezier(0.4, 0, 1, 1)");
    const exit = cubicBezierEasing(0.4, 0, 1, 1);
    // Solved independently by bisection on x(s) = 0.5: s = 0.38096, y(s) = 0.32481.
    expect(exit(0.5)).toBeCloseTo(0.3248, 3);
    expect(exit(0)).toBe(0);
    expect(exit(1)).toBe(1);
  });

  it("is monotonic for the exit curve", () => {
    const exit = cubicBezierEasing(0.4, 0, 1, 1);
    let previous = -1;
    for (let t = 0; t <= 1; t += 0.02) {
      const value = exit(t);
      expect(value).toBeGreaterThanOrEqual(previous);
      previous = value;
    }
  });
});
