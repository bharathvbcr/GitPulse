import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { ACCENTS } from "../accents";
import {
  clampToGamut,
  contrastRatio,
  formatHex,
  formatTriplet,
  inSrgbGamut,
  normalizeHue,
  oklchToRgb,
  parseHex,
  relativeLuminance,
  rgbToOklab,
  rgbToOklch,
  type Rgb,
} from "./oklch";

describe("hex parsing and formatting", () => {
  it("parses #rrggbb and #rgb, any case", () => {
    expect(parseHex("#0b0e17")).toEqual([11, 14, 23]);
    expect(parseHex("#0B0E17")).toEqual([11, 14, 23]);
    expect(parseHex("#fa0")).toEqual([255, 170, 0]);
  });

  it.each([
    "",
    "#",
    "#12",
    "#1234",
    "#12345",
    "#1234567",
    "#12345678",
    "123456",
    "#gggggg",
    " #123456",
    "#123456 ",
    "#123456\n",
    "rgb(1 2 3)",
    "red",
    "#12345g",
  ])("rejects %j", (text) => {
    expect(parseHex(text)).toBeNull();
  });

  it.each([null, undefined, 12, {}, [], NaN, true])("rejects non-string %j", (value) => {
    expect(parseHex(value)).toBeNull();
  });

  it("rejects a huge string without scanning it as a color", () => {
    expect(parseHex(`#${"a".repeat(1_000_000)}`)).toBeNull();
  });

  it("formats lowercase #rrggbb, rounding and clamping", () => {
    expect(formatHex([11, 14, 23])).toBe("#0b0e17");
    expect(formatHex([-5, 300, 127.6])).toBe("#00ff80");
    expect(formatHex([Number.NaN, 0, 0])).toBe("#000000");
  });

  it("formats the --c-* triplet", () => {
    expect(formatTriplet([11, 14, 23])).toBe("11 14 23");
    expect(formatTriplet([300, -1, 0.4])).toBe("255 0 0");
  });
});

describe("OKLab and OKLCH", () => {
  it("matches published reference values", () => {
    // Ottosson's reference: sRGB white is L 1, a ~ b ~ 0; sRGB red is
    // oklab(0.62796 0.22486 0.12585).
    const white = rgbToOklab([255, 255, 255]);
    expect(white.l).toBeCloseTo(1, 4);
    expect(Math.abs(white.a)).toBeLessThan(1e-4);
    const red = rgbToOklab([255, 0, 0]);
    expect(red.l).toBeCloseTo(0.62796, 4);
    expect(red.a).toBeCloseTo(0.22486, 4);
    expect(red.b).toBeCloseTo(0.12585, 4);
    expect(rgbToOklab([0, 0, 0]).l).toBe(0);
  });

  it("round-trips every shipped accent triplet exactly", () => {
    for (const accent of ACCENTS) {
      for (const rgb of [accent.rgb.dark, accent.rgb.light]) {
        expect(oklchToRgb(rgbToOklch(rgb))).toEqual([...rgb]);
      }
    }
  });

  it("round-trips a sweep of the 8-bit cube exactly", () => {
    const steps = [0, 1, 7, 31, 64, 99, 128, 180, 233, 254, 255];
    for (const r of steps) {
      for (const g of steps) {
        for (const b of steps) {
          expect(oklchToRgb(rgbToOklch([r, g, b]))).toEqual([r, g, b]);
        }
      }
    }
  });

  it("gives achromatic colors hue 0 rather than noise", () => {
    expect(rgbToOklch([128, 128, 128]).h).toBe(0);
    expect(rgbToOklch([128, 128, 128]).c).toBeLessThan(1e-3);
  });

  it("wraps hues", () => {
    expect(normalizeHue(-30)).toBe(330);
    expect(normalizeHue(390)).toBe(30);
    expect(normalizeHue(360)).toBe(0);
  });
});

describe("gamut clamp", () => {
  it("leaves in-gamut colors alone", () => {
    const lch = rgbToOklch([20, 26, 41]);
    expect(clampToGamut(lch).c).toBeCloseTo(lch.c, 10);
  });

  it("reduces chroma, never lightness or hue, for an out-of-gamut color", () => {
    const wanted = { l: 0.7, c: 0.4, h: 150 };
    expect(inSrgbGamut(wanted)).toBe(false);
    const clamped = clampToGamut(wanted);
    expect(clamped.l).toBe(0.7);
    expect(clamped.h).toBe(150);
    expect(clamped.c).toBeLessThan(0.4);
    expect(clamped.c).toBeGreaterThan(0.1);
    expect(inSrgbGamut(clamped)).toBe(true);
    // It is the largest in-gamut chroma: a hair more leaves the gamut.
    expect(inSrgbGamut({ ...clamped, c: clamped.c + 0.002 })).toBe(false);
  });

  it("handles lightness outside 0..1 and non-finite parts", () => {
    expect(oklchToRgb({ l: 2, c: 0.3, h: 10 })).toEqual([255, 255, 255]);
    // Gamut slack of 1e-4 linear light allows a channel value of 1 at L 0.
    expect(Math.max(...oklchToRgb({ l: -1, c: 0.3, h: 10 }))).toBeLessThanOrEqual(1);
    expect(oklchToRgb({ l: Number.NaN, c: Number.NaN, h: Number.NaN })).toEqual([0, 0, 0]);
    expect(oklchToRgb({ l: 0.5, c: -1, h: 0 })).toEqual(oklchToRgb({ l: 0.5, c: 0, h: 0 }));
  });
});

describe("WCAG contrast", () => {
  it("has the known extremes", () => {
    expect(contrastRatio([0, 0, 0], [255, 255, 255])).toBeCloseTo(21, 10);
    expect(contrastRatio([9, 9, 9], [9, 9, 9])).toBe(1);
  });

  it("is symmetric", () => {
    const a: Rgb = [128, 158, 255];
    const b: Rgb = [31, 39, 58];
    expect(contrastRatio(a, b)).toBe(contrastRatio(b, a));
  });

  it("agrees with the contract tests' own formula for every accent pair", () => {
    // The formula is local to accents.contract.test.ts (lines 16-28, not
    // exported) and repeated in themeContrast.test.ts (lines 13-25), so it is
    // restated here and the source is checked for the constants that matter.
    const source = readFileSync(new URL("../accents.contract.test.ts", import.meta.url), "utf8");
    expect(source).toContain("value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4");
    expect(source).toContain("0.2126 * r + 0.7152 * g + 0.0722 * b");
    expect(source).toContain("(Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05)");

    const luminance = (rgb: readonly number[]): number => {
      const [r, g, b] = rgb.map((channel) => {
        const value = channel / 255;
        return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
      });
      return 0.2126 * r + 0.7152 * g + 0.0722 * b;
    };
    const reference = (fg: readonly number[], bg: readonly number[]): number => {
      const a = luminance(fg);
      const b = luminance(bg);
      return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
    };
    // WORST_SURFACE, accents.contract.test.ts lines 37-40.
    const worst = { dark: [31, 39, 58], light: [236, 238, 248] } as const;
    for (const accent of ACCENTS) {
      for (const theme of ["dark", "light"] as const) {
        const rgb = accent.rgb[theme];
        expect(contrastRatio(rgb, worst[theme])).toBe(reference(rgb, worst[theme]));
        expect(relativeLuminance(rgb)).toBe(luminance(rgb));
      }
    }
  });
});
