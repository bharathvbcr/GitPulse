import { describe, it, expect, beforeEach, vi } from "vitest";
import { get } from "svelte/store";
import { interfaceStore } from "../interfaceStore";
import { memoryStorage } from "../../repos/persist";

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

  it("customizes graph width without changing the other interface prefs", () => {
    interfaceStore.setShowLanguageBar(false);
    interfaceStore.setGraphWidthMode("wide");
    expect(get(interfaceStore)).toMatchObject({
      showLanguageBar: false,
      showGraphAvatars: true,
      graphWidthMode: "wide",
    });

    interfaceStore.setGraphWidthMode("full");
    expect(get(interfaceStore).graphWidthMode).toBe("full");
  });

  it("reset restores the balanced graph width", () => {
    interfaceStore.setGraphWidthMode("full");
    interfaceStore.reset();
    expect(get(interfaceStore).graphWidthMode).toBe("balanced");
  });

  it("manages font zoom scale and clamps properly", () => {
    expect(get(interfaceStore).uiFontScale).toBe(1.0);
    interfaceStore.zoomIn();
    expect(get(interfaceStore).uiFontScale).toBe(1.05);

    interfaceStore.zoomOut();
    expect(get(interfaceStore).uiFontScale).toBe(1.0);

    interfaceStore.setFontScale(1.3);
    expect(get(interfaceStore).uiFontScale).toBe(1.3);

    interfaceStore.resetZoom();
    expect(get(interfaceStore).uiFontScale).toBe(1.0);
  });

  it("manages coach mark dismissals", () => {
    expect(get(interfaceStore).seenCoachMarks).toEqual({});
    interfaceStore.dismissCoachMark("palette");
    expect(get(interfaceStore).seenCoachMarks["palette"]).toBe(true);

    interfaceStore.resetCoachMarks();
    expect(get(interfaceStore).seenCoachMarks).toEqual({});
  });

  it("leaves the release check opt-in by default", () => {
    interfaceStore.reset();
    const prefs = get(interfaceStore);
    expect(prefs.checkForUpdates).toBe(false);
    expect(prefs.lastUpdateCheckAt).toBe(0);
    expect(prefs.dismissedUpdateVersion).toBe("");
  });

  it("leaves automatic coverage generation opt-in by default", () => {
    // This one runs the repository's own test suites and writes artifacts
    // into the working tree; it must never be on unless the user said so.
    interfaceStore.reset();
    expect(get(interfaceStore).autoRunCoverage).toBe(false);
  });

  it("toggles automatic coverage generation", () => {
    interfaceStore.setAutoRunCoverage(true);
    expect(get(interfaceStore).autoRunCoverage).toBe(true);
    interfaceStore.setAutoRunCoverage(false);
    expect(get(interfaceStore).autoRunCoverage).toBe(false);
  });

  it("refuses to opt a user in from a corrupt stored value", async () => {
    // Anything other than an explicit `true` leaves it off: a partially
    // written blob must not start test suites on the next launch. Prefs are
    // read once when the store is created, so each case needs a fresh module
    // reading a fresh blob. The suite runs without a DOM, so `window` is
    // stubbed with the in-memory storage the app already uses in tests.
    const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
    const load = async (stored: unknown) => {
      const storage = memoryStorage({
        gitpulse_interface_prefs: JSON.stringify({ autoRunCoverage: stored }),
      });
      Object.defineProperty(globalThis, "window", {
        value: { localStorage: storage },
        configurable: true,
        writable: true,
      });
      vi.resetModules();
      return get((await import("../interfaceStore")).interfaceStore).autoRunCoverage;
    };
    try {
      for (const stored of ["true", 1, "yes", {}, [], null]) {
        expect(await load(stored), `stored: ${JSON.stringify(stored)}`).toBe(false);
      }
      // The control: an explicit `true` does opt in, so the cases above are
      // not passing merely because the reader ignores the field.
      expect(await load(true)).toBe(true);
    } finally {
      if (restore) Object.defineProperty(globalThis, "window", restore);
      else delete (globalThis as { window?: unknown }).window;
      vi.resetModules();
    }
  });

  it("toggles the release check and records completed checks", () => {
    interfaceStore.setCheckForUpdates(true);
    expect(get(interfaceStore).checkForUpdates).toBe(true);

    interfaceStore.markUpdateChecked(1_700_000_000_000);
    expect(get(interfaceStore).lastUpdateCheckAt).toBe(1_700_000_000_000);
  });

  it("clears a dismissal when the check is turned off", () => {
    // Re-enabling later must report honestly rather than stay silent about a
    // version dismissed under settings the user has since changed.
    interfaceStore.setCheckForUpdates(true);
    interfaceStore.dismissUpdateVersion("0.1.0");
    expect(get(interfaceStore).dismissedUpdateVersion).toBe("0.1.0");

    interfaceStore.setCheckForUpdates(false);
    expect(get(interfaceStore).dismissedUpdateVersion).toBe("");
  });

  it("keeps a dismissal across an unrelated toggle-on", () => {
    interfaceStore.setCheckForUpdates(true);
    interfaceStore.dismissUpdateVersion("0.1.0");
    interfaceStore.setCheckForUpdates(true);
    expect(get(interfaceStore).dismissedUpdateVersion).toBe("0.1.0");
  });
});
