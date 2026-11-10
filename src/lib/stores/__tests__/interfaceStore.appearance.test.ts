import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

const KEY = "gitpulse_interface_prefs";
const LEGACY = "gitpulse_density_mode";

/** A fresh module graph, so the store reads whatever storage holds right now. */
async function fresh() {
  vi.resetModules();
  const { interfaceStore } = await import("../interfaceStore");
  const { densityStore } = await import("../densityStore");
  return { interfaceStore, densityStore };
}

/** The node test environment has no window: give the store a storage that survives `resetModules`. */
const backing = new Map<string, string>();
const storage = {
  getItem: (key: string) => backing.get(key) ?? null,
  setItem: (key: string, value: string) => void backing.set(key, value),
  removeItem: (key: string) => void backing.delete(key),
  clear: () => backing.clear(),
};

describe("appearance preferences", () => {
  beforeEach(() => {
    backing.clear();
    vi.stubGlobal("window", { localStorage: storage, addEventListener() {}, removeEventListener() {} });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("starts at the shipped look", async () => {
    const { interfaceStore } = await fresh();
    const prefs = get(interfaceStore);
    expect(prefs.density).toBe("comfortable");
    expect(prefs.customAccent).toBe("");
    expect(prefs.customTheme).toBeNull();
    expect(prefs.radiusScale).toBe(1);
    expect(prefs.materialIntensity).toBe(0);
    expect(prefs.floatBlur).toBe(34);
    expect(prefs.fieldLevel).toBe("vivid");
    expect(prefs.reduceTransparency).toBe(false);
  });

  it("migrates the pre-merge density key: spacious stays spacious, compact stays compact", async () => {
    storage.setItem(LEGACY, "compact");
    expect(get((await fresh()).interfaceStore).density).toBe("compact");
    storage.setItem(LEGACY, "spacious");
    expect(get((await fresh()).interfaceStore).density).toBe("spacious");
    storage.removeItem(LEGACY);
    expect(get((await fresh()).interfaceStore).density).toBe("comfortable");
  });

  it("prefers the stored density over the legacy key", async () => {
    storage.setItem(LEGACY, "compact");
    storage.setItem(KEY, JSON.stringify({ density: "spacious" }));
    expect(get((await fresh()).interfaceStore).density).toBe("spacious");
  });

  it("feeds the row tables the two-valued view of the preference", async () => {
    const { interfaceStore, densityStore } = await fresh();
    expect(get(densityStore)).toBe("spacious");
    interfaceStore.setDensity("compact");
    expect(get(densityStore)).toBe("compact");
    interfaceStore.setDensity("comfortable");
    expect(get(densityStore)).toBe("spacious");
    densityStore.toggle();
    expect(get(interfaceStore).density).toBe("compact");
  });

  it("survives a reload", async () => {
    const first = await fresh();
    first.interfaceStore.setDensity("spacious");
    first.interfaceStore.setFloatBlur(12);
    first.interfaceStore.setFieldLevel("subtle");
    first.interfaceStore.setCustomAccent("#3b5bdb");
    first.interfaceStore.setFontMono("JetBrains Mono");
    first.interfaceStore.flush();
    const prefs = get((await fresh()).interfaceStore);
    expect(prefs.density).toBe("spacious");
    expect(prefs.floatBlur).toBe(12);
    expect(prefs.fieldLevel).toBe("subtle");
    expect(prefs.customAccent).toBe("#3b5bdb");
    expect(prefs.fontMono).toBe("JetBrains Mono");
  });

  it("reads corrupt stored values back as defaults, never as themselves", async () => {
    storage.setItem(
      KEY,
      JSON.stringify({ density: "huge", floatBlur: 9000, radiusScale: -4, fontUi: "a;b", customAccent: "red" }),
    );
    const prefs = get((await fresh()).interfaceStore);
    expect(prefs.density).toBe("comfortable");
    expect(prefs.floatBlur).toBe(40);
    expect(prefs.radiusScale).toBe(0.5);
    expect(prefs.fontUi).toBe("");
    expect(prefs.customAccent).toBe("");
  });

  it("clamps the blur setter into 0..40 and rounds it", async () => {
    const { interfaceStore } = await fresh();
    interfaceStore.setFloatBlur(55.4);
    expect(get(interfaceStore).floatBlur).toBe(40);
    interfaceStore.setFloatBlur(-3);
    expect(get(interfaceStore).floatBlur).toBe(0);
    interfaceStore.setFloatBlur(12.6);
    expect(get(interfaceStore).floatBlur).toBe(13);
  });

  it("picking a preset clears a custom accent, so the preset is what shows", async () => {
    const { interfaceStore } = await fresh();
    interfaceStore.setCustomAccent("#3b5bdb");
    interfaceStore.setAccent("rose");
    expect(get(interfaceStore).customAccent).toBe("");
    expect(get(interfaceStore).accent).toBe("rose");
  });

  it("resets Appearance without touching the rest", async () => {
    const { interfaceStore } = await fresh();
    interfaceStore.setDensity("compact");
    interfaceStore.setRadiusScale(0.5);
    interfaceStore.setReduceTransparency(true);
    interfaceStore.setDiffWordWrap(true);
    interfaceStore.resetAppearance();
    const prefs = get(interfaceStore);
    expect(prefs.density).toBe("comfortable");
    expect(prefs.radiusScale).toBe(1);
    expect(prefs.reduceTransparency).toBe(false);
    expect(prefs.diffWordWrap).toBe(true);
  });

  it("reset() restores the appearance preferences with everything else", async () => {
    const { interfaceStore } = await fresh();
    interfaceStore.setDensity("compact");
    interfaceStore.setMaterialIntensity(0.8);
    interfaceStore.reset();
    expect(get(interfaceStore).density).toBe("comfortable");
    expect(get(interfaceStore).materialIntensity).toBe(0);
  });
});
