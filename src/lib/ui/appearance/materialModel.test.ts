import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  SHIPPED_STACK,
  grounds,
  inks,
  materialIntensityFloor,
  measureFloor,
  over,
  thicken,
  worstContrast,
} from "./materialModel";
import { MATERIAL_FLOOR } from "./apply";
import { TEXT_MIN } from "../color/guard";

const css = readFileSync(new URL("../../../app.css", import.meta.url), "utf8");

/** The text of one theme's `html.macos` material block. */
function block(selector: "html.macos" | "html.macos.light"): string {
  const start = css.indexOf(`\n${selector} {\n  /* The hue field`);
  const lightStart = css.indexOf(`\n${selector} {\n  /* Light surfaces invert`);
  const at = selector === "html.macos" ? start : lightStart;
  expect(at, `${selector} material block missing`).toBeGreaterThan(-1);
  return css.slice(at, css.indexOf("\n}", at));
}

describe("the model is the stylesheet", () => {
  it("transcribes the shipped veil and the shipped alphas of every layer", () => {
    const dark = block("html.macos");
    const light = block("html.macos.light");
    expect(dark).toMatch(/--mac-shell-veil:\s*0\.55/);
    expect(light).toMatch(/--mac-shell-veil:\s*0\.62/);
    // The slider thickens `a` to `a + (1 - a) * t`; the stylesheet spells that as calc(a + (1 - a) * var(--gp-material)).
    expect(dark).toContain("calc(0.46 + 0.54 * var(--gp-material, 0))"); // plate
    expect(light).toContain("calc(0.6 + 0.4 * var(--gp-material, 0))"); // plate
    expect(css).toContain("calc(0.42 + 0.58 * var(--gp-material, 0))"); // dark --mac-fill-bg
    expect(css).toContain("calc(0.5 + 0.5 * var(--gp-material, 0))"); // dark --mac-fill-surface
    expect(css).toContain("calc(0.78 + 0.22 * var(--gp-material, 0))"); // dark hover
    expect(css).toContain("calc(0.52 + 0.48 * var(--gp-material, 0))"); // light fill-bg
    expect(css).toContain("calc(0.6 + 0.4 * var(--gp-material, 0))"); // light surface
    expect(css).toContain("calc(0.82 + 0.18 * var(--gp-material, 0))"); // light hover
    const dk = SHIPPED_STACK.dark;
    const lt = SHIPPED_STACK.light;
    expect([dk.plate.alpha, dk.surface.alpha, dk.fillBg.alpha, dk.hover.alpha]).toEqual([0.46, 0.5, 0.42, 0.78]);
    expect([lt.plate.alpha, lt.surface.alpha, lt.fillBg.alpha, lt.hover.alpha]).toEqual([0.6, 0.6, 0.52, 0.82]);
  });

  it("thickening keeps the alpha at intensity 0 and reaches opaque at 1", () => {
    for (const a of [0.42, 0.46, 0.5, 0.6, 0.78, 0.82]) {
      expect(thicken(a, 0)).toBe(a);
      expect(thicken(a, 1)).toBe(1);
    }
  });

  it("composites source-over", () => {
    expect(over([255, 255, 255], 0.5, [0, 0, 0])).toEqual([127.5, 127.5, 127.5]);
    expect(over([10, 20, 30], 1, [200, 200, 200])).toEqual([10, 20, 30]);
  });
});

describe("the measured floor", () => {
  it("covers the extremes: both desktops, every blob, three surface kinds", () => {
    const all = grounds("dark", 0);
    expect(new Set(all.map((ground) => ground.desktop))).toEqual(new Set(["white", "black"]));
    expect(new Set(all.map((ground) => ground.kind))).toEqual(new Set(["panel", "field", "hover"]));
    expect(new Set(all.map((ground) => ground.blob)).size).toBe(5);
  });

  it("holds the dark theme at the shipped alphas over both desktops (the documented 4.9:1)", () => {
    const worst = worstContrast("dark", 0, inks("dark").textMuted);
    expect(worst.ratio).toBeGreaterThanOrEqual(TEXT_MIN);
    expect(worst.ratio).toBeCloseTo(4.93, 1);
    expect(worst.ground.desktop).toBe("white");
    expect(measureFloor("dark").floor).toBe(0);
  });

  it("finds the light theme under AA at the shipped alphas over a black desktop", () => {
    // Design language §14: "light theme over a dark desktop is unmeasured".
    // Measured here: muted text reaches 4.17:1 on a field over black.
    const shipped = measureFloor("light").atShipped.textMuted;
    expect(shipped.ratio).toBeLessThan(TEXT_MIN);
    expect(shipped.ratio).toBeCloseTo(4.17, 1);
    expect(shipped.ground.desktop).toBe("black");
  });

  it("clamps the light theme to the intensity at which every ground passes", () => {
    const light = measureFloor("light");
    expect(light.floor).toBeCloseTo(0.3, 5);
    expect(light.atFloor.text.ratio).toBeGreaterThanOrEqual(TEXT_MIN);
    expect(light.atFloor.textMuted.ratio).toBeGreaterThanOrEqual(TEXT_MIN);
    // One step lower fails: the floor is the least intensity that passes, not a round number.
    const below = worstContrast("light", light.floor - 0.05, inks("light").textMuted);
    expect(below.ratio).toBeLessThan(TEXT_MIN);
  });

  it("is monotone: more opaque never costs contrast", () => {
    for (const appearance of ["dark", "light"] as const) {
      let previous = 0;
      for (let t = 0; t <= 1.0001; t += 0.05) {
        const { ratio } = worstContrast(appearance, t, inks(appearance).textMuted);
        // Rounding of the last step aside, the worst case only improves.
        expect(ratio).toBeGreaterThanOrEqual(previous - 0.15);
        previous = Math.max(previous, ratio);
      }
    }
  });

  it("is what the applier clamps to", () => {
    expect(MATERIAL_FLOOR).toEqual({ dark: 0, light: measureFloor("light").floor });
    expect(materialIntensityFloor()).toBe(MATERIAL_FLOOR.light);
  });
});

describe("a derived palette", () => {
  it("reproduces the shipped floor when it is the shipped palette (parity with the stylesheet model)", async () => {
    const { deriveTokens, SHIPPED_BASE_HEX, SHIPPED_CONTRAST } = await import("../color/derive");
    const { derivedPalette } = await import("./materialModel");
    const { ACCENTS } = await import("../accents");
    const { formatHex } = await import("../color/oklch");
    for (const appearance of ["dark", "light"] as const) {
      const derived = deriveTokens(
        { base: SHIPPED_BASE_HEX[appearance], accent: formatHex(ACCENTS[0].rgb[appearance]), contrast: SHIPPED_CONTRAST },
        appearance,
      );
      if (!derived.ok) throw new Error("shipped palette refused");
      const stack = SHIPPED_STACK[appearance];
      const palette = derivedPalette(appearance, derived.tokens, { blobs: stack.blobs, gain: stack.gain });
      expect(measureFloor(appearance, palette).floor).toBe(measureFloor(appearance).floor);
      expect(worstContrast(appearance, 0, palette.textMuted, palette).ratio).toBeCloseTo(
        worstContrast(appearance, 0, inks(appearance).textMuted).ratio,
        1,
      );
    }
  });

  it("measures a palette with weaker muted text to a higher floor than the shipped one", async () => {
    const { derivedPalette } = await import("./materialModel");
    const stack = SHIPPED_STACK.dark;
    // The shipped dark ladder with its muted ink pulled toward the ground: a
    // theme like that cannot be held to the shipped 0 floor.
    const tokens = {
      "--c-bg": "11 14 23",
      "--c-surface": "20 26 41",
      "--c-surface-hover": "31 39 58",
      "--c-text": "233 237 248",
      "--c-text-muted": "128 140 168",
    };
    const palette = derivedPalette("dark", tokens, { blobs: stack.blobs, gain: stack.gain });
    expect(measureFloor("dark", palette).floor).toBeGreaterThan(measureFloor("dark").floor);
  });
});
