import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { interfaceStore } from "../interfaceStore";
import { themeStore } from "../themeStore";
import { presetById } from "../../ui/appearance/presets";
import { MAX_SAVED_THEMES } from "../../ui/appearance/savedThemes";
import { APPEARANCE_DEFAULTS } from "../../ui/appearance/apply";
import { themeToPatch } from "../../ui/appearance/themeFile";
import { THEME_SCHEMA_ID, validateTheme, type GitPulseTheme } from "../../ui/appearance/themeSchema";

function theme(name: string, appearance: "dark" | "light" = "dark"): GitPulseTheme {
  const checked = validateTheme({ schema: THEME_SCHEMA_ID, name, appearance, colors: { base: "#101418", accent: "#4cc9ff" } });
  if (!checked.ok) throw new Error("fixture invalid");
  return checked.theme;
}

describe("interfaceStore theme presets and the theme shelf", () => {
  beforeEach(() => {
    interfaceStore.reset();
    themeStore.setPreference("system");
  });

  it("applies a preset's theme, wallpaper and material, and switches to its appearance", () => {
    interfaceStore.setAccent("teal");
    interfaceStore.applyPreset(presetById("paper")!);
    const prefs = get(interfaceStore);
    expect(prefs.customTheme?.name).toBe("Paper");
    expect(prefs.accent).toBe("blue");
    expect(prefs.backdrop).toBe("none");
    expect(themeStore.preference()).toBe("light");
    interfaceStore.applyPreset(presetById("neural-hud")!);
    expect(get(interfaceStore).backdrop).toBe("neural");
    expect(themeStore.preference()).toBe("dark");
  });

  it("applying a saved theme leaves the wallpaper as it is", () => {
    interfaceStore.setBackdrop("circuit");
    interfaceStore.applyTheme(theme("Mine", "light"));
    expect(get(interfaceStore).backdrop).toBe("circuit");
    expect(get(interfaceStore).customTheme?.name).toBe("Mine");
    expect(themeStore.preference()).toBe("light");
  });

  it("saves, replaces, refuses when full and removes", () => {
    expect(interfaceStore.saveTheme(theme("A"))).toBe("added");
    expect(interfaceStore.saveTheme(theme("a"))).toBe("replaced");
    expect(get(interfaceStore).savedThemes.map((entry) => entry.name)).toEqual(["a"]);
    for (let index = 1; index < MAX_SAVED_THEMES; index += 1) interfaceStore.saveTheme(theme(`T${index}`));
    expect(get(interfaceStore).savedThemes).toHaveLength(MAX_SAVED_THEMES);
    expect(interfaceStore.saveTheme(theme("One too many"))).toBe("full");
    expect(get(interfaceStore).savedThemes).toHaveLength(MAX_SAVED_THEMES);
    interfaceStore.removeSavedTheme({ name: "A", appearance: "dark" });
    expect(get(interfaceStore).savedThemes.some((entry) => entry.name === "a")).toBe(false);
  });

  it("removing a preset takes back its wallpaper and material too", () => {
    interfaceStore.applyPreset(presetById("neural-hud")!);
    interfaceStore.clearTheme();
    let prefs = get(interfaceStore);
    expect(prefs.customTheme).toBeNull();
    expect(prefs.backdrop).toBe("none");

    interfaceStore.applyPreset(presetById("pure-black")!);
    expect(get(interfaceStore).materialIntensity.dark).toBe(1);
    interfaceStore.clearTheme();
    prefs = get(interfaceStore);
    expect(prefs.customTheme).toBeNull();
    expect(prefs.materialIntensity.dark).toBe(0);
  });

  it("removing a preset keeps a wallpaper or material chosen after it", () => {
    interfaceStore.applyPreset(presetById("neural-hud")!);
    interfaceStore.setBackdrop("hex");
    interfaceStore.setMaterialIntensity("dark", 0.4);
    interfaceStore.clearTheme();
    const prefs = get(interfaceStore);
    expect(prefs.customTheme).toBeNull();
    expect(prefs.backdrop).toBe("hex");
    expect(prefs.materialIntensity.dark).toBe(0.4);
  });

  it("removing a saved theme leaves the wallpaper, and undoes the corner scale and fonts it brought", () => {
    const checked = validateTheme({
      schema: THEME_SCHEMA_ID,
      name: "Dusk",
      appearance: "dark",
      colors: { base: "#14101f", accent: "#c08cff" },
      shape: { radiusScale: 1.25 },
      fonts: { ui: "Helvetica Neue", mono: "Menlo" },
    });
    if (!checked.ok) throw new Error("fixture invalid");
    interfaceStore.setBackdrop("circuit");
    interfaceStore.patchAppearance(themeToPatch(checked.theme));
    interfaceStore.clearTheme();
    const prefs = get(interfaceStore);
    expect(prefs.customTheme).toBeNull();
    expect(prefs.backdrop).toBe("circuit");
    expect(prefs.radiusScale).toBe(APPEARANCE_DEFAULTS.radiusScale);
    expect(prefs.fontUi).toBe("");
    expect(prefs.fontMono).toBe("");
  });

  it("removing a theme keeps a font the reader picked after it", () => {
    const checked = validateTheme({
      schema: THEME_SCHEMA_ID,
      name: "Dusk",
      appearance: "dark",
      colors: { base: "#14101f", accent: "#c08cff" },
      fonts: { ui: "Helvetica Neue", mono: "Menlo" },
    });
    if (!checked.ok) throw new Error("fixture invalid");
    interfaceStore.patchAppearance(themeToPatch(checked.theme));
    interfaceStore.patchAppearance({ fontMono: "Monaco" });
    interfaceStore.clearTheme();
    expect(get(interfaceStore).fontMono).toBe("Monaco");
    expect(get(interfaceStore).fontUi).toBe("");
  });

  it("removing with no theme in force changes nothing", () => {
    interfaceStore.setBackdrop("hex");
    const before = JSON.stringify(get(interfaceStore));
    interfaceStore.clearTheme();
    expect(JSON.stringify(get(interfaceStore))).toBe(before);
  });

  it("Reset appearance keeps the saved themes but drops the wallpaper and the applied theme", () => {
    interfaceStore.saveTheme(theme("Keep me"));
    interfaceStore.applyPreset(presetById("crimson")!);
    interfaceStore.resetAppearance();
    const prefs = get(interfaceStore);
    expect(prefs.savedThemes.map((entry) => entry.name)).toEqual(["Keep me"]);
    expect(prefs.backdrop).toBe("none");
    expect(prefs.customTheme).toBeNull();
  });
});
