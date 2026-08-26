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

  it("defaults to showing graph avatars", () => {
    expect(get(interfaceStore).showGraphAvatars).toBe(true);
  });

  it("toggles graph avatars via setter and toggle, preserving other prefs", () => {
    interfaceStore.setShowLanguageBar(false);
    interfaceStore.setShowGraphAvatars(false);
    let prefs = get(interfaceStore);
    expect(prefs.showGraphAvatars).toBe(false);
    expect(prefs.showLanguageBar).toBe(false);

    interfaceStore.toggleGraphAvatars();
    prefs = get(interfaceStore);
    expect(prefs.showGraphAvatars).toBe(true);
    expect(prefs.showLanguageBar).toBe(false);

    interfaceStore.toggleGraphAvatars();
    expect(get(interfaceStore).showGraphAvatars).toBe(false);
  });

  it("reset restores the avatar default too", () => {
    interfaceStore.setShowGraphAvatars(false);
    interfaceStore.reset();
    expect(get(interfaceStore).showGraphAvatars).toBe(true);
  });
});
