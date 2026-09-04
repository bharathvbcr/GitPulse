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

  it("starts with the Fleet dashboard closed", () => {
    interfaceStore.reset();
    expect(get(interfaceStore).fleetOpen).toBe(false);
  });

  it("opens, closes and toggles the Fleet dashboard", () => {
    interfaceStore.reset();
    interfaceStore.setFleetOpen(true);
    expect(get(interfaceStore).fleetOpen).toBe(true);
    interfaceStore.toggleFleet();
    expect(get(interfaceStore).fleetOpen).toBe(false);
    interfaceStore.toggleFleet();
    expect(get(interfaceStore).fleetOpen).toBe(true);
    interfaceStore.setFleetOpen(false);
    expect(get(interfaceStore).fleetOpen).toBe(false);
  });

  it("persists the Fleet surface across a reload", async () => {
    // Fleet lives here rather than in the workspace blob precisely so it can
    // be remembered without bumping that schema version — which would make an
    // older build fall back to its legacy keys and lose the user's tabs.
    const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
    try {
      const storage = memoryStorage({
        gitpulse_interface_prefs: JSON.stringify({ fleetOpen: true }),
      });
      Object.defineProperty(globalThis, "window", {
        value: { localStorage: storage },
        configurable: true,
        writable: true,
      });
      vi.resetModules();
      const reloaded = (await import("../interfaceStore")).interfaceStore;
      expect(get(reloaded).fleetOpen).toBe(true);
    } finally {
      if (restore) Object.defineProperty(globalThis, "window", restore);
      else Reflect.deleteProperty(globalThis, "window");
      vi.resetModules();
    }
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

describe("interfaceStore chrome preferences", () => {
  beforeEach(() => {
    interfaceStore.reset();
  });

  it("starts with every piece of chrome present", () => {
    // Defaults must not quietly hide anything: a fresh install shows the
    // whole frame, and decluttering is something the user chooses.
    const prefs = get(interfaceStore);
    expect(prefs.hiddenViews).toEqual([]);
    expect(prefs.statusBarMode).toBe("full");
    expect(prefs.showHeaderActionLabels).toBe(true);
    expect(prefs.autoHideRepoTabs).toBe(false);
    expect(prefs.diagnosticsButton).toBe("always");
  });

  it("sets each chrome preference without disturbing the others", () => {
    interfaceStore.setStatusBarMode("hidden");
    interfaceStore.setShowHeaderActionLabels(false);
    interfaceStore.setAutoHideRepoTabs(true);
    interfaceStore.setDiagnosticsButton("issues");
    const prefs = get(interfaceStore);
    expect(prefs.statusBarMode).toBe("hidden");
    expect(prefs.showHeaderActionLabels).toBe(false);
    expect(prefs.autoHideRepoTabs).toBe(true);
    expect(prefs.diagnosticsButton).toBe("issues");
    // Untouched neighbours from other sections stay put.
    expect(prefs.showLanguageBar).toBe(true);
    expect(prefs.uiFontScale).toBe(1.0);
  });

  it("hides and re-shows individual views without duplicating entries", () => {
    interfaceStore.setViewHidden("code", true);
    interfaceStore.setViewHidden("code", true);
    interfaceStore.setViewHidden("insights", true);
    expect(get(interfaceStore).hiddenViews).toEqual(["code", "insights"]);

    interfaceStore.setViewHidden("code", false);
    expect(get(interfaceStore).hiddenViews).toEqual(["insights"]);

    // Un-hiding something that was never hidden is a no-op, not an error.
    interfaceStore.setViewHidden("work", false);
    expect(get(interfaceStore).hiddenViews).toEqual(["insights"]);

    interfaceStore.showAllViews();
    expect(get(interfaceStore).hiddenViews).toEqual([]);
  });

  it("reset restores the chrome defaults and cannot poison them", () => {
    interfaceStore.setViewHidden("code", true);
    interfaceStore.setStatusBarMode("minimal");
    interfaceStore.reset();
    expect(get(interfaceStore).hiddenViews).toEqual([]);
    expect(get(interfaceStore).statusBarMode).toBe("full");

    // A second cycle proves the first reset handed out its own array rather
    // than the module-level default everyone would then share.
    interfaceStore.setViewHidden("insights", true);
    interfaceStore.reset();
    expect(get(interfaceStore).hiddenViews).toEqual([]);
  });

  it("falls back rather than trusting a corrupt chrome blob", async () => {
    const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
    const load = async (stored: Record<string, unknown>) => {
      const storage = memoryStorage({
        gitpulse_interface_prefs: JSON.stringify(stored),
      });
      Object.defineProperty(globalThis, "window", {
        value: { localStorage: storage },
        configurable: true,
        writable: true,
      });
      vi.resetModules();
      return get((await import("../interfaceStore")).interfaceStore);
    };
    try {
      const bad = await load({
        statusBarMode: "off",
        diagnosticsButton: "errors",
        hiddenViews: ["code", "blame", "not-a-view", 7, null],
        showHeaderActionLabels: "no",
        autoHideRepoTabs: 1,
      });
      expect(bad.statusBarMode).toBe("full");
      expect(bad.diagnosticsButton).toBe("always");
      // "blame" is a retired view id: it survives in old preference blobs and
      // must be dropped like any other non-view, or the preference would name
      // something the header can never list.
      expect(bad.hiddenViews).toEqual(["code"]);
      expect(bad.showHeaderActionLabels).toBe(true);
      expect(bad.autoHideRepoTabs).toBe(false);

      // The control: valid values do survive, so the cases above are not
      // passing because the reader ignores these fields.
      const good = await load({
        statusBarMode: "minimal",
        diagnosticsButton: "issues",
        hiddenViews: ["insights"],
        showHeaderActionLabels: false,
        autoHideRepoTabs: true,
      });
      expect(good.statusBarMode).toBe("minimal");
      expect(good.diagnosticsButton).toBe("issues");
      expect(good.hiddenViews).toEqual(["insights"]);
      expect(good.showHeaderActionLabels).toBe(false);
      expect(good.autoHideRepoTabs).toBe(true);
    } finally {
      if (restore) Object.defineProperty(globalThis, "window", restore);
      else delete (globalThis as { window?: unknown }).window;
      vi.resetModules();
    }
  });
});
