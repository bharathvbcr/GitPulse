import { readFileSync, readdirSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { tokenClass } from "../files/syntaxHighlight";
import { ACCENTS } from "./accents";
import { TEXT_MIN } from "./color/guard";
import { SHIPPED_BASE_HEX, SHIPPED_CONTRAST, deriveTokens, type TokenKey } from "./color/derive";
import { contrastRatio, formatHex, inSrgbGamut, oklchToRgb, type Rgb } from "./color/oklch";

const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");
const lightCss = css.match(/html\.light\s*\{([\s\S]*?)\n\}/)?.[1] ?? "";

function rgbVariable(name: string): [number, number, number] {
  const match = lightCss.match(new RegExp(`--${name}:\\s*(\\d+)\\s+(\\d+)\\s+(\\d+)`));
  if (!match) throw new Error(`missing --${name}`);
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}

function luminance(rgb: readonly number[]): number {
  const [r, g, b] = rgb.map((channel) => {
    const value = channel / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(foreground: readonly number[], background: readonly number[]): number {
  const a = luminance(foreground);
  const b = luminance(background);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

function composite(foreground: readonly number[], background: readonly number[], alpha: number) {
  return foreground.map((channel, index) =>
    Math.round(channel * alpha + background[index] * (1 - alpha)),
  );
}

describe("light-theme contrast contract", () => {
  const lightHover: [number, number, number] = [236, 238, 248];

  it("keeps ordinary text and accent text above WCAG AA on the darkest light surface", () => {
    expect(contrast(rgbVariable("c-text-muted"), lightHover)).toBeGreaterThanOrEqual(4.5);
    expect(contrast(rgbVariable("c-accent"), lightHover)).toBeGreaterThanOrEqual(4.5);
  });

  it("keeps 80%-opacity control borders distinguishable at 3:1", () => {
    const rendered = composite(rgbVariable("c-border"), lightHover, 0.8);
    expect(contrast(rendered, lightHover)).toBeGreaterThanOrEqual(3);
  });

  it.each([
    "amber",
    "emerald",
    "rose",
    "red",
    "sky",
    "cyan",
    "purple",
    "green",
    "orange",
    "teal",
    "indigo",
    "blue",
    "yellow",
    "zinc",
    "slate",
  ])("keeps %s status text above WCAG AA", (name) => {
    expect(contrast(rgbVariable(`c-status-${name}`), lightHover)).toBeGreaterThanOrEqual(4.5);
  });

  it("has no class-attribute remap of text colour, so a theme is only ever the tokens", () => {
    expect(css).not.toMatch(/\[class~=["']text-/);
  });

  it("emits no translucent muted or accent text, which the light theme used to patch up with a remap", () => {
    // `text-textMuted/60` is the muted colour at 60% alpha: below 4.5:1 on a
    // light surface by construction. Text that wants to be quieter than
    // muted uses `text-faint`, which is opaque and held to 4.5:1 below.
    const offenders: string[] = [];
    const walk = (dir: URL) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const child = new URL(entry.name + (entry.isDirectory() ? "/" : ""), dir);
        if (entry.isDirectory()) walk(child);
        else if (/\.(svelte|ts)$/.test(entry.name) && !/\.test\.ts$/.test(entry.name)) {
          const text = readFileSync(child, "utf8");
          const hit = text.match(/(?<![\w:\-/[])text-(?:textMuted\/(?:40|50|60|70|80)|accent\/(?:60|70|80))(?![\w/])/);
          if (hit) offenders.push(`${entry.name}: ${hit[0]}`);
        }
      }
    };
    walk(new URL("../../", import.meta.url));
    expect(offenders).toEqual([]);
  });
});

/**
 * The foundation roles (design language §4.1) held to the §4.3 guard, read from
 * the stylesheet that ships so a hand edit of a triplet is caught, and compared
 * with deriveTokens() so the CSS and the derivation cannot drift apart.
 */
describe("foundation color roles meet the contrast guard in both themes", () => {
  const darkStart = css.indexOf("html.dark {");
  const darkCss = css.slice(darkStart, css.indexOf("\n  }", darkStart));
  const blocks = { dark: darkCss, light: lightCss } as const;
  const ROLES = ["ok", "warn", "danger", "info", "unknown"] as const;

  function triplet(theme: "dark" | "light", name: string): Rgb {
    const match = blocks[theme].match(new RegExp(`--${name}:\\s*(\\d+)\\s+(\\d+)\\s+(\\d+)`));
    if (!match) throw new Error(`missing --${name} in the ${theme} theme`);
    return [Number(match[1]), Number(match[2]), Number(match[3])];
  }

  /** Straight-alpha composite, unrounded: the browser does not round it either. */
  function over(foreground: Rgb, background: Rgb, alpha: number): Rgb {
    return [
      foreground[0] * alpha + background[0] * (1 - alpha),
      foreground[1] * alpha + background[1] * (1 - alpha),
      foreground[2] * alpha + background[2] * (1 - alpha),
    ];
  }

  it("finds the dark block it is meant to be checking", () => {
    expect(darkStart).toBeGreaterThan(0);
    expect(darkCss).toContain("--c-bg:");
    expect(darkCss).toContain("--c-ok:");
  });

  describe.each(["dark", "light"] as const)("%s theme", (theme) => {
    const surfaces = ["c-bg", "c-surface", "c-surface-hover"].map((name) => triplet(theme, name));

    it.each([...ROLES, "accent"])("keeps %s text at 4.5:1 on every surface", (role) => {
      for (const surface of surfaces) {
        expect(contrastRatio(triplet(theme, `c-${role}`), surface)).toBeGreaterThanOrEqual(TEXT_MIN);
      }
    });

    it("keeps faint text at 4.5:1 on every surface", () => {
      for (const surface of surfaces) {
        expect(contrastRatio(triplet(theme, "c-text-faint"), surface)).toBeGreaterThanOrEqual(TEXT_MIN);
      }
    });

    it.each([...ROLES, "accent"])("keeps the ink on a %s fill at 4.5:1", (role) => {
      expect(contrastRatio(triplet(theme, `c-on-${role}`), triplet(theme, `c-${role}`))).toBeGreaterThanOrEqual(TEXT_MIN);
    });

    it.each(ROLES)("keeps %s text readable on its own 10% chip tint over every surface", (role) => {
      // 10% is the strongest tint that holds 4.5:1 for every role in both
      // themes (measured worst case 4.57); 15% dropped light `ok` to 4.30.
      const color = triplet(theme, `c-${role}`);
      for (const surface of surfaces) {
        expect(contrastRatio(color, over(color, surface, 0.1))).toBeGreaterThanOrEqual(TEXT_MIN);
      }
    });

    it("paints every gp-chip and icon-button tint at the 10% this test measures", () => {
      const chip = css.slice(css.indexOf("@utility gp-chip"), css.indexOf("@utility gp-floor"));
      const iconButton = css.slice(css.indexOf("@utility gp-icon-btn"), css.indexOf("@utility gp-pill"));
      const tints = [...(chip + iconButton).matchAll(/background-color: rgb\(var\(--c-[a-z]+\) \/ ([\d.]+)\)/g)].map(
        (match) => match[1],
      );
      expect(tints.length).toBeGreaterThanOrEqual(7);
      expect([...new Set(tints)]).toEqual(["0.1"]);
    });

    it("paints the selected row at the 12% this test measures", () => {
      const row = css.slice(css.indexOf("@utility gp-row"), css.indexOf("@utility gp-label"));
      expect(row).toMatch(/\[data-selected="true"\]\s*\{\s*background-color: rgb\(var\(--c-accent\) \/ 0\.12\)/);
    });

    it("keeps accent text readable on a selected row's 12% accent tint over every surface", () => {
      const accent = triplet(theme, "c-accent");
      for (const surface of surfaces) {
        expect(contrastRatio(accent, over(accent, surface, 0.12))).toBeGreaterThanOrEqual(TEXT_MIN);
      }
    });

    describe("syntax roles", () => {
      // The code background (viewer pane, panels it can sit in) plus the paints
      // that land under code: the added and removed row tints the diff viewer
      // uses (bg-ok/15, bg-danger/15) and the selected-line accent tint.
      const codeBackgrounds: Rgb[] = [
        ...surfaces,
        ...surfaces.flatMap((surface) => [
          over(triplet(theme, "c-ok"), surface, 0.15),
          over(triplet(theme, "c-danger"), surface, 0.15),
          over(triplet(theme, "c-accent"), surface, 0.15),
        ]),
      ];

      function syntaxColour(name: string): Rgb {
        const match = blocks[theme].match(
          new RegExp(`--c-syntax-${name}:\\s*oklch\\(([\\d.]+)\\s+([\\d.]+)\\s+([\\d.]+)\\)`),
        );
        if (!match) throw new Error(`missing --c-syntax-${name} (as oklch) in the ${theme} theme`);
        const lch = { l: Number(match[1]), c: Number(match[2]), h: Number(match[3]) };
        // The measurement only describes what a browser paints if the colour
        // needs no gamut mapping, so a token is held inside sRGB.
        expect(inSrgbGamut(lch), `--c-syntax-${name} must be inside sRGB`).toBe(true);
        return oklchToRgb(lch);
      }

      const names = [
        "keyword", "type", "string", "number", "function", "operator", "attribute", "property", "regex", "variable",
        "punctuation",
      ];

      it("defines every syntax role in the theme, and a Tailwind colour for each", () => {
        expect(names).toHaveLength(11);
        for (const name of names) {
          expect(blocks[theme], name).toContain(`--c-syntax-${name}:`);
          expect(css, name).toContain(`--color-syntax-${name}: var(--c-syntax-${name});`);
        }
      });

      it.each(names)("keeps %s at 4.5:1 on the code background, the diff row tints and a selected line", (name) => {
        const colour = syntaxColour(name);
        for (const bg of codeBackgrounds) {
          expect(contrastRatio(colour, bg), `${name} on rgb(${bg.map(Math.round).join(" ")})`).toBeGreaterThanOrEqual(TEXT_MIN);
        }
      });

      it("resolves every colour class tokenClass returns to a defined role", () => {
        const types = [
          "keyword", "type", "string", "comment", "number", "function", "operator", "tag", "attribute",
          "property", "punctuation", "regex", "variable", "text",
        ] as const;
        for (const type of types) {
          const colour = tokenClass(type).split(/\s+/).find((token) => token.startsWith("text-"));
          const role = colour?.match(/^text-syntax-([a-z]+)$/)?.[1];
          if (role) expect(names, `${type} -> ${colour}`).toContain(role);
          else expect(colour, type).toMatch(/^text-(faint|textPrimary\/90)$/);
        }
      });
    });

    it("keeps the default on-accent ink readable on every preset accent, so a preset needs no ink of its own", () => {
      const ink = triplet(theme, "c-on-accent");
      for (const preset of ACCENTS) {
        expect(contrastRatio(ink, preset.rgb[theme]), `${preset.id} (${theme})`).toBeGreaterThanOrEqual(TEXT_MIN);
      }
    });

    it("is what deriveTokens produces for the shipped base (the CSS has not drifted from the derivation)", () => {
      const result = deriveTokens(
        { base: SHIPPED_BASE_HEX[theme], accent: formatHex(triplet(theme, "c-accent")), contrast: SHIPPED_CONTRAST },
        theme,
      );
      expect(result.ok).toBe(true);
      if (!result.ok) return;
      const names = ["c-text-faint", "c-on-accent", ...ROLES.flatMap((role) => [`c-${role}`, `c-on-${role}`])];
      for (const name of names) {
        // The key is built from a name this test wrote, so a typo fails as undefined.
        expect(triplet(theme, name).join(" "), `--${name}`).toBe(result.tokens[`--${name}` as TokenKey]);
      }
    });
  });
});
