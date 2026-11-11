import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { ACCENTS } from "../accents";
import { TEXT_MIN } from "./guard";
import {
  CODE_TINT_ALPHA,
  SHIPPED_BASE_HEX,
  SHIPPED_CONTRAST,
  SHIPPED_SYNTAX,
  SYNTAX_KEYS,
  deriveTokens,
  type Appearance,
  type DeriveInputs,
  type SyntaxKey,
} from "./derive";
import { clampToGamut, contrastRatio, formatHex, oklchToRgb, type Rgb } from "./oklch";

const css = readFileSync(new URL("../../../app.css", import.meta.url), "utf8");

function triplet(text: string): Rgb {
  const [r, g, b] = text.split(" ").map(Number);
  return [r, g, b];
}

function rgbFunction(text: string): Rgb {
  const match = text.match(/^rgb\((\d+) (\d+) (\d+)\)$/);
  if (!match) throw new Error(`not an rgb() colour: ${text}`);
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}

function mix(fill: Rgb, surface: Rgb): Rgb {
  const at = (i: 0 | 1 | 2) => fill[i] * CODE_TINT_ALPHA + surface[i] * (1 - CODE_TINT_ALPHA);
  return [at(0), at(1), at(2)];
}

/** Every paint code is read over, built from a derived palette's own tokens. */
function codeBackgrounds(tokens: Record<string, string>): Rgb[] {
  const surfaces = ["--c-bg", "--c-surface", "--c-surface-hover"].map((key) => triplet(tokens[key]));
  return [
    ...surfaces,
    ...surfaces.flatMap((surface) =>
      ["--c-ok", "--c-danger", "--c-accent"].map((key) => mix(triplet(tokens[key]), surface)),
    ),
  ];
}

function themeBlock(appearance: Appearance): string {
  const start = css.indexOf(`html.${appearance} {`);
  return css.slice(start, css.indexOf("\n  }", start));
}

describe("shipped syntax colours (the starting point for a custom palette)", () => {
  it.each(["dark", "light"] as const)("matches the %s block of app.css, role for role", (appearance) => {
    const block = themeBlock(appearance);
    for (const key of SYNTAX_KEYS) {
      const match = block.match(new RegExp(`${key}:\\s*oklch\\(([\\d.]+)\\s+([\\d.]+)\\s+([\\d.]+)\\)`));
      expect(match, `${key} in ${appearance}`).not.toBeNull();
      if (!match) continue;
      expect(SHIPPED_SYNTAX[appearance][key]).toEqual({ l: Number(match[1]), c: Number(match[2]), h: Number(match[3]) });
    }
  });

  it.each(["dark", "light"] as const)("is left alone by the guard on the shipped %s base", (appearance) => {
    const result = deriveTokens(
      { base: SHIPPED_BASE_HEX[appearance], accent: formatHex(triplet(appearance === "dark" ? "128 158 255" : "48 64 175")), contrast: SHIPPED_CONTRAST },
      appearance,
    );
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    for (const key of SYNTAX_KEYS) {
      expect(result.report.find((entry) => entry.key === key)?.adjusted, key).toBe(false);
    }
  });
});

describe("syntax colours under a custom palette", () => {
  // A light base in dark mode: the shipped dark syntax colours are pale, and the
  // surfaces are now paler still. Text roles are overridden to dark inks, as an
  // imported theme would do, so that the palette itself derives.
  const lightInDark: DeriveInputs = {
    base: "#e6e6ee",
    accent: "#3040af",
    contrast: SHIPPED_CONTRAST,
    text: "#101018",
    textMuted: "#3a3a48",
  };

  it("is only a real test because the shipped colours fail on that background", () => {
    const result = deriveTokens(lightInDark, "dark");
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    const backgrounds = codeBackgrounds(result.tokens);
    const stranded = SYNTAX_KEYS.filter((key) => {
      const shipped = oklchToRgb(clampToGamut(SHIPPED_SYNTAX.dark[key]));
      return Math.min(...backgrounds.map((bg) => contrastRatio(shipped, bg))) < TEXT_MIN;
    });
    expect(stranded.length).toBe(SYNTAX_KEYS.length);
  });

  it("holds every applied syntax colour at 4.5:1 on every derived code background (very light base, dark mode)", () => {
    const result = deriveTokens(lightInDark, "dark");
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    const backgrounds = codeBackgrounds(result.tokens);
    expect(Object.keys(result.syntax).sort()).toEqual([...SYNTAX_KEYS].sort());
    for (const key of SYNTAX_KEYS) {
      const colour = rgbFunction(result.syntax[key]);
      for (const bg of backgrounds) {
        expect(contrastRatio(colour, bg), `${key} on rgb(${bg.map(Math.round).join(" ")})`).toBeGreaterThanOrEqual(TEXT_MIN);
      }
      expect(result.report.find((entry) => entry.key === key)?.adjusted, `${key} should have been moved`).toBe(true);
    }
  });

  it("never returns stale syntax colours for a very dark base in light mode: the palette is refused or the colours pass", () => {
    const result = deriveTokens(
      { base: "#0b0e17", accent: "#809eff", contrast: SHIPPED_CONTRAST, text: "#f4f4fa", textMuted: "#c8cadc" },
      "light",
    );
    if (!result.ok) {
      expect(result.failures.length).toBeGreaterThan(0);
      return;
    }
    const backgrounds = codeBackgrounds(result.tokens);
    for (const key of SYNTAX_KEYS) {
      for (const bg of backgrounds) expect(contrastRatio(rgbFunction(result.syntax[key]), bg), key).toBeGreaterThanOrEqual(TEXT_MIN);
    }
  });

  it.each(["dark", "light"] as const)("holds on every preset accent's selected-line tint (%s)", (appearance) => {
    for (const preset of ACCENTS) {
      const result = deriveTokens(
        { base: SHIPPED_BASE_HEX[appearance], accent: formatHex(preset.rgb[appearance]), contrast: SHIPPED_CONTRAST },
        appearance,
      );
      expect(result.ok, preset.id).toBe(true);
      if (!result.ok) continue;
      const backgrounds = codeBackgrounds(result.tokens);
      for (const key of SYNTAX_KEYS as readonly SyntaxKey[]) {
        for (const bg of backgrounds) {
          expect(contrastRatio(rgbFunction(result.syntax[key]), bg), `${preset.id} ${key}`).toBeGreaterThanOrEqual(TEXT_MIN);
        }
      }
    }
  });
});
