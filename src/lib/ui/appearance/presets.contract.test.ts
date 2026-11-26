import { describe, expect, it } from "vitest";
import { deriveTokens, SHIPPED_CONTRAST } from "../color/derive";
import { TEXT_MIN } from "../color/guard";
import { contrastRatio } from "../color/oklch";
import { APPEARANCE_DEFAULTS, planAppearance, type AppearancePrefs } from "./apply";
import { isBackdropId } from "./backdrops";
import { derivedPalette, measureFloor, shippedPalette, worstContrast } from "./materialModel";
import { THEME_PRESETS, presetById, presetPatch, sameTheme, themeSelectionPatch } from "./presets";
import { validateTheme } from "./themeSchema";
import { parseHex } from "../color/oklch";

/**
 * Every built-in preset, checked the way an imported theme is: it validates,
 * derives without a refusal, draws its own accent, and the window can reach a
 * readable material for it over any desktop. Cases come from THEME_PRESETS, so
 * adding a preset adds its checks.
 */

function applied(id: string): AppearancePrefs {
  const preset = presetById(id);
  if (!preset) throw new Error(`no preset ${id}`);
  return { ...APPEARANCE_DEFAULTS, ...presetPatch(preset, APPEARANCE_DEFAULTS) };
}

describe("the preset list", () => {
  it("has unique ids and labels and covers the requested looks", () => {
    expect(new Set(THEME_PRESETS.map((preset) => preset.id)).size).toBe(THEME_PRESETS.length);
    expect(new Set(THEME_PRESETS.map((preset) => preset.label)).size).toBe(THEME_PRESETS.length);
    for (const id of ["cobalt", "lodestar", "oxblood", "gilded", "neural-hud", "reactor", "pure-black", "crimson"]) {
      expect(presetById(id)).toBeDefined();
    }
    // At least one light preset, so the gallery is not dark-only.
    expect(THEME_PRESETS.some((preset) => preset.theme.appearance === "light")).toBe(true);
  });

  it("Pure Black is black and draws no wallpaper or glow; Crimson is black with a red accent", () => {
    const black = presetById("pure-black")!;
    expect(black.theme.colors.base).toBe("#000000");
    expect(black.backdrop).toBe("none");
    expect(black.theme.field?.gain).toBe(0);
    expect(black.material).toBe(1);
    const crimson = presetById("crimson")!;
    const [r, g, b] = parseHex(crimson.theme.colors.accent)!;
    expect(r).toBeGreaterThan(200);
    expect(Math.max(g, b)).toBeLessThan(120);
    expect(parseHex(crimson.theme.colors.base)!.every((channel) => channel < 16)).toBe(true);
  });
});

describe.each(THEME_PRESETS.map((preset) => [preset.id, preset] as const))("preset %s", (_id, preset) => {
  it("is exactly what the theme-file validator accepts", () => {
    const checked = validateTheme(JSON.parse(JSON.stringify(preset.theme)));
    expect(checked.ok).toBe(true);
    if (checked.ok) {
      expect(checked.ignoredKeys).toEqual([]);
      expect(sameTheme(checked.theme, preset.theme)).toBe(true);
    }
    expect(isBackdropId(preset.backdrop)).toBe(true);
    expect(preset.material).toBeGreaterThanOrEqual(0);
    expect(preset.material).toBeLessThanOrEqual(1);
  });

  it("derives a full palette in its own appearance with no refusal", () => {
    const result = deriveTokens(
      {
        base: preset.theme.colors.base,
        accent: preset.theme.colors.accent,
        contrast: preset.theme.colors.contrast ?? SHIPPED_CONTRAST,
      },
      preset.theme.appearance,
    );
    expect(result.ok, result.ok ? "" : JSON.stringify(result.failures)).toBe(true);
    if (!result.ok) return;
    const bg = result.tokens["--c-bg"].split(" ").map(Number) as [number, number, number];
    const text = result.tokens["--c-text"].split(" ").map(Number) as [number, number, number];
    const muted = result.tokens["--c-text-muted"].split(" ").map(Number) as [number, number, number];
    expect(contrastRatio(text, bg)).toBeGreaterThanOrEqual(TEXT_MIN);
    expect(contrastRatio(muted, bg)).toBeGreaterThanOrEqual(TEXT_MIN);
  });

  it("applies without a refusal note, switching nothing the window must undo", () => {
    const prefs = applied(preset.id);
    const plan = planAppearance(prefs, { theme: preset.theme.appearance, nativeMaterial: true });
    expect(plan.notes.filter((note) => note.id === "theme-refused" || note.id === "theme-other-mode")).toEqual([]);
    // The preset's palette is what draws: the surfaces were written.
    expect(plan.vars["--c-bg"]).toBeDefined();
    expect(plan.accentOverride).not.toBeNull();
    if (preset.backdrop !== "none") {
      expect(plan.vars["--gp-backdrop"]).toMatch(/^url\("data:image\/svg\+xml,/);
      expect(plan.attributes["data-backdrop"]).toBe(preset.backdrop);
    } else {
      expect(plan.vars["--gp-backdrop"]).toBeUndefined();
    }
  });

  it("has a material floor the slider can reach, and is readable over any desktop at it", () => {
    const prefs = applied(preset.id);
    const plan = planAppearance(prefs, { theme: preset.theme.appearance, nativeMaterial: true });
    expect(plan.materialFloor).toBeGreaterThanOrEqual(0);
    expect(plan.materialFloor).toBeLessThanOrEqual(1);
    const result = deriveTokens(
      {
        base: preset.theme.colors.base,
        accent: preset.theme.colors.accent,
        contrast: preset.theme.colors.contrast ?? SHIPPED_CONTRAST,
      },
      preset.theme.appearance,
    );
    if (!result.ok) throw new Error("derive refused");
    const field = preset.theme.field!;
    const palette = derivedPalette(preset.theme.appearance, result.tokens, {
      blobs: field.hues.map((hex) => parseHex(hex)!),
      gain: (preset.theme.appearance === "dark" ? 1 : 0.5) * field.gain,
    });
    const intensity = Math.max(preset.material, plan.materialFloor);
    for (const ink of [palette.text, palette.textMuted]) {
      expect(worstContrast(preset.theme.appearance, intensity, ink, palette).ratio).toBeGreaterThanOrEqual(TEXT_MIN);
    }
    expect(measureFloor(preset.theme.appearance, palette).floor).toBe(plan.materialFloor);
  });
});

describe("a wallpaper is any picture under the veil", () => {
  /**
   * The model proves the stack over a white and a black desktop and argues
   * that those extremes bound any picture. That argument is only as good as
   * the claim that no picture in between is worse, so it is checked here
   * directly: greys, saturated primaries and secondaries, under every blob,
   * for the shipped palette and every preset, at the floor the window draws.
   */
  const pictures: [number, number, number][] = [];
  for (let v = 0; v <= 255; v += 17) pictures.push([v, v, v]);
  for (const r of [0, 255]) for (const g of [0, 255]) for (const b of [0, 255]) pictures.push([r, g, b]);
  pictures.push([255, 128, 0], [128, 0, 255], [0, 200, 120], [200, 30, 60]);

  const cases = [
    { id: "shipped dark", prefs: APPEARANCE_DEFAULTS, appearance: "dark" as const },
    { id: "shipped light", prefs: APPEARANCE_DEFAULTS, appearance: "light" as const },
    ...THEME_PRESETS.map((preset) => ({ id: preset.id, prefs: applied(preset.id), appearance: preset.theme.appearance })),
  ];

  it.each(cases.map((entry) => [entry.id, entry] as const))("%s stays readable over every picture", (_id, entry) => {
    const plan = planAppearance(entry.prefs, { theme: entry.appearance, nativeMaterial: true });
    const t = Math.max(plan.materialFloor, entry.prefs.materialIntensity[entry.appearance]);
    const triplet = (key: string): [number, number, number] => {
      const value = plan.vars[key];
      return value === undefined ? (null as never) : (value.split(" ").map(Number) as [number, number, number]);
    };
    const preset = presetById(entry.id);
    let palette;
    if (preset) {
      const result = deriveTokens(
        { base: preset.theme.colors.base, accent: preset.theme.colors.accent, contrast: preset.theme.colors.contrast ?? SHIPPED_CONTRAST },
        entry.appearance,
      );
      if (!result.ok) throw new Error("derive refused");
      palette = derivedPalette(entry.appearance, result.tokens, {
        blobs: preset.theme.field!.hues.map((hex) => parseHex(hex)!),
        gain: (entry.appearance === "dark" ? 1 : 0.5) * preset.theme.field!.gain,
      });
      expect(triplet("--c-bg")).toEqual(palette.shell.map(Math.round));
    }
    const p = palette ?? shippedPalette(entry.appearance);
    const { stack } = p;
    const thick = (a: number) => a + (1 - a) * t;
    const mix = (c: readonly number[], a: number, under: readonly number[]) => c.map((v, i) => v * a + under[i] * (1 - a));
    let worst = Number.POSITIVE_INFINITY;
    for (const picture of pictures) {
      const shell = mix(p.shell, stack.veil, picture);
      for (const blob of [null, ...stack.blobs]) {
        const field = blob === null ? shell : mix(blob, stack.blobAlpha * stack.gain, shell);
        const plate = mix(stack.plate.color, thick(stack.plate.alpha), field);
        const panel = mix(stack.surface.color, thick(stack.surface.alpha), plate);
        const inset = mix(stack.fillBg.color, thick(stack.fillBg.alpha), plate);
        const hover = mix(stack.hover.color, thick(stack.hover.alpha), panel);
        for (const ground of [panel, inset, hover]) {
          for (const ink of [p.text, p.textMuted]) {
            worst = Math.min(worst, contrastRatio(ink, ground as [number, number, number]));
          }
        }
      }
    }
    expect(worst).toBeGreaterThanOrEqual(TEXT_MIN);
  });
});

describe("applying from the gallery", () => {
  it("resets a leftover accent so the theme's own accent draws", () => {
    const preset = presetById("crimson")!;
    const before: AppearancePrefs = { ...APPEARANCE_DEFAULTS, accent: "teal", customAccent: "#123456" };
    const after = { ...before, ...presetPatch(preset, before) };
    expect(after.accent).toBe(APPEARANCE_DEFAULTS.accent);
    expect(after.customAccent).toBe("");
    const plan = planAppearance(after, { theme: "dark", nativeMaterial: true });
    const accent = plan.accentOverride!.split(" ").map(Number);
    // Red channel dominant: Crimson's accent, not teal or the custom blue.
    expect(accent[0]).toBeGreaterThan(accent[1] + 80);
  });

  it("sets material only for the preset's own appearance", () => {
    const preset = presetById("pure-black")!;
    const before: AppearancePrefs = { ...APPEARANCE_DEFAULTS, materialIntensity: { light: 0.4, dark: 0.2 } };
    expect(presetPatch(preset, before).materialIntensity).toEqual({ light: 0.4, dark: 1 });
  });

  it("a saved theme carries its corner scale and leaves the wallpaper alone", () => {
    const preset = presetById("neural-hud")!;
    const patch = themeSelectionPatch({ ...preset.theme, shape: { radiusScale: 0.5 } });
    expect(patch.radiusScale).toBe(0.5);
    expect("backdrop" in patch).toBe(false);
  });
});
