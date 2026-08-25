import { describe, it, expect, beforeEach } from "vitest";
import { get } from "svelte/store";
import { interfaceStore } from "../interfaceStore";

describe("interfaceStore", () => {
  beforeEach(() => {
    interfaceStore.reset();
  });

  it("defaults to showing the language bar and harness badges", () => {
    const prefs = get(interfaceStore);
    expect(prefs.showLanguageBar).toBe(true);
    expect(prefs.showHarnessBadges).toBe(true);
  });

  it("hides and re-shows the language bar independently", () => {
    interfaceStore.setShowLanguageBar(false);
    expect(get(interfaceStore).showLanguageBar).toBe(false);

    interfaceStore.setShowHarnessBadges(false);
    expect(get(interfaceStore).showHarnessBadges).toBe(false);
    expect(get(interfaceStore).showLanguageBar).toBe(false);

    interfaceStore.setShowLanguageBar(true);
    expect(get(interfaceStore).showLanguageBar).toBe(true);
    expect(get(interfaceStore).showHarnessBadges).toBe(false);
  });

  it("reset restores both defaults", () => {
    interfaceStore.setShowLanguageBar(false);
    interfaceStore.setShowHarnessBadges(false);
    interfaceStore.reset();
    const prefs = get(interfaceStore);
    expect(prefs.showLanguageBar).toBe(true);
    expect(prefs.showHarnessBadges).toBe(true);
  });
});
