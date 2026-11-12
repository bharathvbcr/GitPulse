import { describe, expect, it } from "vitest";
import {
  MAX_THEME_FILE_BYTES,
  exportCurrentTheme,
  importThemeText,
  themeFileName,
  themeToPatch,
} from "./themeFile";
import { APPEARANCE_DEFAULTS, planAppearance, type AppearancePrefs } from "./apply";
import { validateTheme } from "./themeSchema";

function prefs(changes: Partial<AppearancePrefs> = {}): AppearancePrefs {
  return { ...APPEARANCE_DEFAULTS, ...changes };
}

describe("export then import", () => {
  it("round-trips the window as it is", () => {
    const exported = exportCurrentTheme(
      prefs({ radiusScale: 1.25, fontUi: "Inter", fontMono: "JetBrains Mono", fieldHue: 40, fieldLevel: "subtle", customAccent: "#3b5bdb" }),
      "dark",
      "Round and blue",
    );
    expect(exported.ok).toBe(true);
    if (!exported.ok) return;
    expect(exported.fileName).toBe("round-and-blue.gitpulse-theme.json");
    const imported = importThemeText(exported.json);
    expect(imported.ok).toBe(true);
    if (!imported.ok) return;
    expect(imported.theme).toEqual(exported.theme);
    expect(imported.ignoredKeys).toEqual([]);
    expect(imported.theme.shape?.radiusScale).toBe(1.25);
    expect(imported.theme.fonts).toEqual({ ui: "Inter", mono: "JetBrains Mono" });
    expect(imported.theme.field?.gain).toBe(0.5);
  });

  it("writes the guard's result, not the colour that was asked for", () => {
    const exported = exportCurrentTheme(prefs({ customAccent: "#1d2a52" }), "dark", "Dim");
    expect(exported.ok).toBe(true);
    if (!exported.ok) return;
    expect(exported.theme.colors.accent).not.toBe("#1d2a52");
  });

  it("leaves out shape and fonts the window does not customize", () => {
    const exported = exportCurrentTheme(prefs(), "light", "Plain");
    expect(exported.ok).toBe(true);
    if (!exported.ok) return;
    expect(exported.theme.shape).toBeUndefined();
    expect(exported.theme.fonts).toBeUndefined();
    expect(exported.theme.appearance).toBe("light");
  });

  it("carries an applied theme's palette through, so it imports back to the same palette", () => {
    const first = exportCurrentTheme(prefs(), "dark", "Seed");
    if (!first.ok) throw new Error("fixture");
    const custom = { ...first.theme, colors: { ...first.theme.colors, base: "#101a2b", contrast: 0.6 } };
    const again = exportCurrentTheme(prefs({ customTheme: custom }), "dark", "Again");
    expect(again.ok).toBe(true);
    if (!again.ok) return;
    expect(again.theme.colors.base).toBe("#101a2b");
    expect(again.theme.colors.contrast).toBe(0.6);
  });
});

describe("importing an untrusted file", () => {
  const good = {
    schema: "gitpulse-theme/1",
    name: "Dusk",
    appearance: "dark",
    colors: { base: "#14101f", accent: "#c08cff" },
  };

  it("accepts a small valid file", () => {
    const result = importThemeText(JSON.stringify(good));
    expect(result.ok).toBe(true);
  });

  it.each([
    ["a CSS string in a colour", { ...good, colors: { ...good.colors, accent: "red; background: url(//evil)" } }],
    ["url() in the name", { ...good, name: "url(https://evil.example/x)" }],
    ["a filter key", { ...good, filter: "blur(4px)" }],
    ["a blur key", { ...good, blur: 40 }],
    ["a z-index key", { ...good, zIndex: 9999 }],
    ["a stylesheet key", { ...good, css: "body{display:none}" }],
    ["a font URL", { ...good, fonts: { ui: "https://evil.example/f.woff2", mono: "Menlo" } }],
    ["a CSS function in a font", { ...good, fonts: { ui: "Inter", mono: "var(--x)" } }],
    ["the wrong schema", { ...good, schema: "gitpulse-theme/2" }],
    ["a missing accent", { ...good, colors: { base: "#14101f" } }],
  ])("refuses %s, and says why", (_label, file) => {
    const result = importThemeText(JSON.stringify(file));
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.errors.length).toBeGreaterThan(0);
    expect(result.reason.length).toBeGreaterThan(0);
  });

  it("refuses text that is not JSON", () => {
    const result = importThemeText("not json {");
    expect(result).toMatchObject({ ok: false, reason: "The file isn't valid JSON." });
  });

  it("refuses a file larger than a theme needs without parsing it", () => {
    const result = importThemeText(" ".repeat(MAX_THEME_FILE_BYTES + 1));
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.reason).toContain("larger than");
  });

  it("reports unknown keys but still applies the rest", () => {
    const result = importThemeText(JSON.stringify({ ...good, flavor: "mint" }));
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.ignoredKeys).toEqual(["flavor"]);
  });

  it("never half-applies: a failed import returns no theme at all", () => {
    const result = importThemeText(JSON.stringify({ ...good, colors: { base: "#14101f", accent: 7 } }));
    expect(result.ok).toBe(false);
    expect("theme" in result).toBe(false);
  });
});

describe("themeToPatch", () => {
  it("brings the theme's shape and fonts along and drops a stale custom accent", () => {
    const parsed = validateTheme({
      schema: "gitpulse-theme/1",
      name: "Soft",
      appearance: "dark",
      colors: { base: "#14101f", accent: "#c08cff" },
      shape: { radiusScale: 0.5 },
      fonts: { ui: "Inter", mono: "ui-monospace" },
    });
    if (!parsed.ok) throw new Error("fixture");
    const patch = themeToPatch(parsed.theme);
    expect(patch.customAccent).toBe("");
    expect(patch.radiusScale).toBe(0.5);
    expect(patch.fontUi).toBe("Inter");
    // "ui-monospace" is the file's word for the system monospace font.
    expect(patch.fontMono).toBe("");
    expect(patch.customTheme).toBe(parsed.theme);
  });

  it("applied, the imported palette reaches the plan", () => {
    const parsed = validateTheme({
      schema: "gitpulse-theme/1",
      name: "Soft",
      appearance: "dark",
      colors: { base: "#14101f", accent: "#c08cff" },
    });
    if (!parsed.ok) throw new Error("fixture");
    const plan = planAppearance(prefs(themeToPatch(parsed.theme)), { theme: "dark", nativeMaterial: true });
    expect(plan.vars["--c-bg"]).toBeDefined();
  });
});

describe("themeFileName", () => {
  it("slugifies the name", () => {
    const parsed = validateTheme({
      schema: "gitpulse-theme/1",
      name: "My  Théme (v2)",
      appearance: "dark",
      colors: { base: "#14101f", accent: "#c08cff" },
    });
    if (!parsed.ok) throw new Error("fixture");
    expect(themeFileName(parsed.theme)).toBe("my-théme-v2.gitpulse-theme.json");
  });
});
