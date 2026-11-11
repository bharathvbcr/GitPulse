import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { ACCENTS } from "../accents";
import {
  SHIPPED_BASE_HEX,
  SHIPPED_CONTRAST,
  SHIPPED_LADDER,
  STATUS_HUES,
  TOKEN_KEYS,
  deriveTokens,
  ladderOffsets,
  type Appearance,
  type DeriveInputs,
  type TokenKey,
} from "./derive";
import { NON_TEXT_MIN, TEXT_MIN } from "./guard";
import { contrastRatio, formatHex, parseHex, rgbToOklch, type Rgb } from "./oklch";

const css = readFileSync(new URL("../../../app.css", import.meta.url), "utf8");

/** Same extraction as accents.contract.test.ts: a `html.dark` / `html.light` declaration block. */
function themeBlock(selector: string): string {
  const re = new RegExp(`(?:^|[\\n,])\\s*${selector.replaceAll(".", "\\.")}\\s*\\{`);
  const match = re.exec(css);
  expect(match, `${selector} theme block missing`).toBeTruthy();
  const start = match!.index;
  return css.slice(start, css.indexOf("\n}", start));
}

function cssTriplet(selector: string, name: string): Rgb {
  const match = themeBlock(selector).match(new RegExp(`--${name}:\\s*(\\d+)\\s+(\\d+)\\s+(\\d+)`));
  if (!match) throw new Error(`missing --${name} in ${selector}`);
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}

const SELECTOR: Record<Appearance, string> = { dark: "html.dark", light: "html.light" };

function triplet(text: string): Rgb {
  const [r, g, b] = text.split(" ").map(Number);
  return [r, g, b];
}

function inputsFor(appearance: Appearance, overrides: Partial<DeriveInputs> = {}): DeriveInputs {
  const accent = ACCENTS[0].rgb[appearance];
  return {
    base: SHIPPED_BASE_HEX[appearance],
    accent: formatHex(accent),
    contrast: SHIPPED_CONTRAST,
    ...overrides,
  };
}

function derive(appearance: Appearance, overrides: Partial<DeriveInputs> = {}) {
  const result = deriveTokens(inputsFor(appearance, overrides), appearance);
  if (!result.ok) throw new Error(`derive failed: ${JSON.stringify(result.failures)}`);
  return result;
}

describe.each(["dark", "light"] as const)("the shipped %s ladder", (appearance) => {
  const shipped = SHIPPED_LADDER[appearance];

  it("is what app.css declares (the transcription has not drifted)", () => {
    const selector = SELECTOR[appearance];
    expect(shipped.bg).toEqual(cssTriplet(selector, "c-bg"));
    expect(shipped.surface).toEqual(cssTriplet(selector, "c-surface"));
    expect(shipped.surfaceHover).toEqual(cssTriplet(selector, "c-surface-hover"));
    expect(shipped.border).toEqual(cssTriplet(selector, "c-border"));
    expect(shipped.text).toEqual(cssTriplet(selector, "c-text"));
    expect(shipped.textMuted).toEqual(cssTriplet(selector, "c-text-muted"));
    expect(parseHex(SHIPPED_BASE_HEX[appearance])).toEqual(cssTriplet(selector, "c-bg"));
  });

  it("is reproduced EXACTLY for bg, surface, surface-hover and border at contrast 0.5", () => {
    const { tokens } = derive(appearance);
    const selector = SELECTOR[appearance];
    expect(triplet(tokens["--c-bg"])).toEqual(cssTriplet(selector, "c-bg"));
    expect(triplet(tokens["--c-surface"])).toEqual(cssTriplet(selector, "c-surface"));
    expect(triplet(tokens["--c-surface-hover"])).toEqual(cssTriplet(selector, "c-surface-hover"));
    expect(triplet(tokens["--c-border"])).toEqual(cssTriplet(selector, "c-border"));
  });

  it("reproduces text and text-muted within 1 per channel (measured: exactly)", () => {
    const { tokens } = derive(appearance);
    const selector = SELECTOR[appearance];
    for (const [token, name] of [
      ["--c-text", "c-text"],
      ["--c-text-muted", "c-text-muted"],
    ] as const) {
      const got = triplet(tokens[token]);
      const want = cssTriplet(selector, name);
      for (let channel = 0; channel < 3; channel += 1) {
        expect(Math.abs(got[channel] - want[channel])).toBeLessThanOrEqual(1);
      }
      expect(got).toEqual(want);
    }
  });

  it("measures the ladder from the triplets rather than from tuned constants", () => {
    const offsets = ladderOffsets(appearance);
    const bg = rgbToOklch(shipped.bg);
    for (const [role, rgb] of [
      ["surface", shipped.surface],
      ["surfaceHover", shipped.surfaceHover],
      ["border", shipped.border],
      ["text", shipped.text],
      ["textMuted", shipped.textMuted],
    ] as const) {
      expect(bg.l + offsets[role].dl).toBeCloseTo(rgbToOklch(rgb).l, 12);
    }
  });
});

describe("derive: the contrast input", () => {
  it("moves the ladder monotonically in dark: 0 collapses it, 1 doubles it", () => {
    const l = (contrast: number): number[] => {
      const { tokens } = derive("dark", { contrast });
      return ["--c-bg", "--c-surface", "--c-surface-hover", "--c-border"].map(
        (key) => rgbToOklch(triplet(tokens[key as TokenKey])).l,
      );
    };
    const [flat0, flat1, flat2, flat3] = l(0);
    expect(flat1).toBeCloseTo(flat0, 2);
    expect(flat2).toBeCloseTo(flat0, 2);
    expect(flat3).toBeCloseTo(flat0, 2);
    const half = l(0.5);
    const full = l(1);
    for (let i = 1; i < 4; i += 1) {
      expect(half[i]).toBeGreaterThan(flat0);
      expect(full[i] - full[0]).toBeGreaterThan(half[i] - half[0]);
    }
  });

  it("changes only the surfaces' spacing, and the guard still holds at every contrast", () => {
    for (const appearance of ["dark", "light"] as const) {
      for (const contrast of [0, 0.1, 0.25, 0.5, 0.75, 1]) {
        const result = deriveTokens(inputsFor(appearance, { contrast }), appearance);
        expect(result.ok, `${appearance} ${contrast}`).toBe(true);
      }
    }
  });
});

describe.each(["dark", "light"] as const)("derive: guarded roles (%s)", (appearance) => {
  it("keeps every text, accent and status color at 4.5:1 on all three surfaces", () => {
    const { tokens } = derive(appearance);
    const surfaces = ["--c-bg", "--c-surface", "--c-surface-hover"].map((key) => triplet(tokens[key as TokenKey]));
    for (const key of [
      "--c-text",
      "--c-text-muted",
      "--c-text-faint",
      "--c-accent",
      "--c-ok",
      "--c-warn",
      "--c-danger",
      "--c-info",
      "--c-unknown",
    ] as const) {
      for (const surface of surfaces) {
        expect(contrastRatio(triplet(tokens[key]), surface), `${key} on ${surface}`).toBeGreaterThanOrEqual(TEXT_MIN);
      }
    }
  });

  it("also clears the 3:1 non-text bar for the same roles", () => {
    const { tokens } = derive(appearance);
    for (const key of ["--c-accent", "--c-ok", "--c-warn", "--c-danger", "--c-info", "--c-unknown"] as const) {
      expect(contrastRatio(triplet(tokens[key]), triplet(tokens["--c-surface-hover"]))).toBeGreaterThanOrEqual(NON_TEXT_MIN);
    }
  });

  it("puts a readable on-* ink on every fill", () => {
    const { tokens } = derive(appearance);
    for (const role of ["accent", "ok", "warn", "danger", "info", "unknown"] as const) {
      const fill = triplet(tokens[`--c-${role}` as TokenKey]);
      const ink = triplet(tokens[`--c-on-${role}` as TokenKey]);
      expect(contrastRatio(ink, fill), `on-${role}`).toBeGreaterThanOrEqual(TEXT_MIN);
    }
  });

  it("emits every declared token as an R G B triplet of 0..255", () => {
    const { tokens } = derive(appearance);
    expect(Object.keys(tokens).sort()).toEqual([...TOKEN_KEYS].sort());
    for (const key of TOKEN_KEYS) expect(tokens[key]).toMatch(/^(?:\d{1,3}) (?:\d{1,3}) (?:\d{1,3})$/);
    for (const key of TOKEN_KEYS) for (const channel of triplet(tokens[key])) expect(channel).toBeLessThanOrEqual(255);
  });

  it("gives status colors their spec hues (within gamut-clamp and guard drift) and unknown none", () => {
    const { tokens } = derive(appearance);
    for (const role of ["ok", "warn", "danger", "info"] as const) {
      const lch = rgbToOklch(triplet(tokens[`--c-${role}` as TokenKey]));
      const diff = Math.abs(((lch.h - STATUS_HUES[role] + 540) % 360) - 180);
      expect(diff, role).toBeLessThan(6);
    }
    expect(rgbToOklch(triplet(tokens["--c-unknown"])).c).toBeLessThan(0.01);
  });

  it("separates the four status colors from each other", () => {
    const { tokens } = derive(appearance);
    const seen = new Set(["--c-ok", "--c-warn", "--c-danger", "--c-info", "--c-unknown"].map((key) => tokens[key as TokenKey]));
    expect(seen.size).toBe(5);
  });

  it("orders faint between muted and the surface", () => {
    const { tokens } = derive(appearance);
    const ratio = (key: TokenKey): number => contrastRatio(triplet(tokens[key]), triplet(tokens["--c-surface-hover"]));
    expect(ratio("--c-text")).toBeGreaterThan(ratio("--c-text-muted"));
    expect(ratio("--c-text-muted")).toBeGreaterThanOrEqual(ratio("--c-text-faint"));
  });
});

describe("derive: accent and overrides", () => {
  it("leaves a legible accent exactly as given, and reports it as not adjusted", () => {
    const result = derive("dark");
    expect(result.tokens["--c-accent"]).toBe("128 158 255");
    expect(result.report.find((entry) => entry.key === "--c-accent")?.adjusted).toBe(false);
  });

  it("nudges an illegible accent and says so", () => {
    const result = derive("light", { accent: "#ffe08a" });
    const entry = result.report.find((e) => e.key === "--c-accent");
    expect(entry?.adjusted).toBe(true);
    expect(contrastRatio(triplet(result.tokens["--c-accent"]), triplet(result.tokens["--c-surface-hover"]))).toBeGreaterThanOrEqual(TEXT_MIN);
  });

  it("names the key and ratio when the guard refuses", () => {
    // A mid grey base leaves no lightness range that clears 4.5:1 for an accent lighter than it.
    const result = deriveTokens({ base: "#888888", accent: "#898989", contrast: 0 }, "dark");
    expect(result.ok).toBe(false);
    if (result.ok) return;
    const accent = result.failures.find((f) => f.key === "--c-accent");
    expect(accent).toBeDefined();
    expect(accent?.ratio).toBeLessThan(TEXT_MIN);
    expect(accent?.minimum).toBe(TEXT_MIN);
  });

  it("honours a status override and still guards it", () => {
    const result = derive("dark", { ok: "#1a3d24" });
    expect(contrastRatio(triplet(result.tokens["--c-ok"]), triplet(result.tokens["--c-surface-hover"]))).toBeGreaterThanOrEqual(TEXT_MIN);
    expect(result.report.find((e) => e.key === "--c-ok")?.adjusted).toBe(true);
  });

  it("caps a vivid base's chroma so the ladder stays a grey", () => {
    const { tokens } = derive("dark", { base: "#14003d" });
    expect(rgbToOklch(parseHex("#14003d") as Rgb).c).toBeGreaterThan(0.1);
    // 8-bit rounding moves chroma by a few thousandths.
    expect(rgbToOklch(triplet(tokens["--c-bg"])).c).toBeLessThanOrEqual(0.033);
  });

  it("derives a ladder around a non-shipped base, keeping its hue", () => {
    const { tokens } = derive("dark", { base: "#1a0f14" });
    const base = rgbToOklch(parseHex("#1a0f14") as Rgb);
    expect(Math.abs(rgbToOklch(triplet(tokens["--c-surface"])).h - base.h)).toBeLessThan(15);
  });
});

describe("derive: adversarial inputs", () => {
  const bad = (partial: Partial<Record<keyof DeriveInputs, unknown>>) =>
    deriveTokens({ ...inputsFor("dark"), ...partial } as DeriveInputs, "dark");

  it.each([
    ["empty base", { base: "" }, "base"],
    ["non-hex accent", { accent: "rgb(1 2 3)" }, "accent"],
    ["alpha hex", { accent: "#11223344" }, "accent"],
    ["huge accent", { accent: `#${"f".repeat(100000)}` }, "accent"],
    ["number base", { base: 123456 }, "base"],
    ["null accent", { accent: null }, "accent"],
    ["NaN contrast", { contrast: Number.NaN }, "contrast"],
    ["Infinity contrast", { contrast: Number.POSITIVE_INFINITY }, "contrast"],
    ["negative contrast", { contrast: -0.1 }, "contrast"],
    ["contrast above 1", { contrast: 1.01 }, "contrast"],
    ["string contrast", { contrast: "0.5" }, "contrast"],
    ["bad override", { ok: "green" }, "ok"],
    ["object override", { warn: {} }, "warn"],
  ])("refuses %s and names it", (_label, partial, key) => {
    const result = bad(partial);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.failures.map((f) => f.key)).toContain(key);
  });

  it("reports every bad input at once", () => {
    const result = bad({ base: "x", accent: "y", contrast: 7 });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.failures.map((f) => f.key).sort()).toEqual(["accent", "base", "contrast"]);
  });

  it("handles extreme but valid bases without throwing", () => {
    for (const base of ["#000", "#fff", "#000000", "#ffffff", "#ff00ff", "#00ff00"]) {
      for (const appearance of ["dark", "light"] as const) {
        expect(() => deriveTokens({ base, accent: "#3355ff", contrast: 0.5 }, appearance)).not.toThrow();
      }
    }
  });
});
