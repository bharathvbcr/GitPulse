import { describe, expect, it } from "vitest";
import { ACCENTS } from "../accents";
import {
  LIGHTNESS_CEILING,
  LIGHTNESS_FLOOR,
  NON_TEXT_MIN,
  TEXT_MIN,
  ensureContrast,
} from "./guard";
import { contrastRatio, rgbToOklch, type Rgb } from "./oklch";

const DARK_WORST: Rgb = [31, 39, 58];
const LIGHT_WORST: Rgb = [236, 238, 248];

describe("thresholds", () => {
  it("are the WCAG AA numbers", () => {
    expect(TEXT_MIN).toBe(4.5);
    expect(NON_TEXT_MIN).toBe(3.0);
  });
});

describe("ensureContrast", () => {
  it("returns a passing color untouched, flagged as not adjusted", () => {
    const result = ensureContrast([233, 237, 248], DARK_WORST, TEXT_MIN);
    expect(result).toMatchObject({ ok: true, adjusted: false, color: [233, 237, 248] });
  });

  it("passes every shipped accent unchanged on its worst surface", () => {
    for (const accent of ACCENTS) {
      expect(ensureContrast(accent.rgb.dark, DARK_WORST, TEXT_MIN)).toMatchObject({ ok: true, adjusted: false });
      expect(ensureContrast(accent.rgb.light, LIGHT_WORST, TEXT_MIN)).toMatchObject({ ok: true, adjusted: false });
    }
  });

  it("lightens a too-dark foreground on a dark surface and keeps its hue", () => {
    const fg: Rgb = [60, 70, 150];
    const before = rgbToOklch(fg);
    const result = ensureContrast(fg, DARK_WORST, TEXT_MIN);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.adjusted).toBe(true);
    expect(result.ratio).toBeGreaterThanOrEqual(TEXT_MIN);
    expect(contrastRatio(result.color, DARK_WORST)).toBe(result.ratio);
    const after = rgbToOklch(result.color);
    expect(after.l).toBeGreaterThan(before.l);
    expect(Math.abs(after.h - before.h)).toBeLessThan(4);
  });

  it("darkens a too-light foreground on a light surface", () => {
    const fg: Rgb = [150, 170, 240];
    const result = ensureContrast(fg, LIGHT_WORST, TEXT_MIN);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(rgbToOklch(result.color).l).toBeLessThan(rgbToOklch(fg).l);
    expect(result.ratio).toBeGreaterThanOrEqual(TEXT_MIN);
  });

  it("works for the 3:1 non-text bar too", () => {
    const result = ensureContrast([44, 54, 78], DARK_WORST, NON_TEXT_MIN);
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.ratio).toBeGreaterThanOrEqual(NON_TEXT_MIN);
  });

  it("refuses when the background leaves no room, and reports the ratio it reached", () => {
    // Mid grey on mid grey: even L 95% / 5% cannot make 21:1.
    const result = ensureContrast([120, 120, 120], [128, 128, 128], 21);
    expect(result.ok).toBe(false);
    expect(result.ratio).toBeLessThan(21);
    expect(result.ratio).toBeGreaterThan(1);
  });

  it("refuses a foreground already beyond the lightness ceiling", () => {
    // Lighter than the background and past L 95%: nowhere further to go.
    const result = ensureContrast([255, 255, 255], [250, 250, 250], 4.5);
    expect(result.ok).toBe(false);
    expect(result.ratio).toBeCloseTo(contrastRatio([255, 255, 255], [250, 250, 250]), 10);
  });

  it("never returns ok with a color below the minimum", () => {
    const fgs: Rgb[] = [[0, 0, 0], [255, 255, 255], [128, 128, 128], [255, 0, 0], [0, 255, 0], [31, 39, 58]];
    const bgs: Rgb[] = [DARK_WORST, LIGHT_WORST, [128, 128, 128], [0, 0, 0], [255, 255, 255]];
    for (const fg of fgs) {
      for (const bg of bgs) {
        for (const min of [1, 3, 4.5, 7]) {
          const result = ensureContrast(fg, bg, min);
          if (result.ok) {
            expect(contrastRatio(result.color, bg)).toBeGreaterThanOrEqual(min);
            expect(result.ratio).toBe(contrastRatio(result.color, bg));
          } else {
            // A refusal means the color really was out of reach at the limit.
            expect(result.ratio).toBeLessThan(min);
          }
        }
      }
    }
  });

  it("exposes lightness limits of 5% and 95%", () => {
    expect(LIGHTNESS_FLOOR).toBe(0.05);
    expect(LIGHTNESS_CEILING).toBe(0.95);
  });

  it.each([Number.NaN, 0.5, 22, Number.POSITIVE_INFINITY])("throws on a bad minimum %s", (min) => {
    expect(() => ensureContrast([1, 2, 3], DARK_WORST, min)).toThrow(RangeError);
  });

  it("throws on non-finite channels instead of reporting a pass", () => {
    expect(() => ensureContrast([Number.NaN, 0, 0], DARK_WORST, 4.5)).toThrow(RangeError);
    expect(() => ensureContrast([0, 0, 0], [Number.POSITIVE_INFINITY, 0, 0], 4.5)).toThrow(RangeError);
    // @ts-expect-error a short tuple is not an RGB triplet
    expect(() => ensureContrast([0, 0], DARK_WORST, 4.5)).toThrow(RangeError);
  });
});
