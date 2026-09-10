import { describe, it, expect, beforeEach, vi } from "vitest";
import { get } from "svelte/store";
import { interfaceStore } from "../interfaceStore";
import { memoryStorage } from "../../repos/persist";

describe("interfaceStore", () => {
  it("selects one global surface and preserves the terminal dock", () => {
    interfaceStore.setTerminalDockOpen(true);
    interfaceStore.setGlobalSurface("tasks");
    expect(get(interfaceStore).globalSurface).toBe("tasks");
    interfaceStore.toggleFleet();
    expect(get(interfaceStore).globalSurface).toBe("fleet");
    interfaceStore.setFleetOpen(false);
    expect(get(interfaceStore).globalSurface).toBe("repository");
    expect(get(interfaceStore).terminalDockOpen).toBe(true);
  });
  beforeEach(() => {
    interfaceStore.reset();
  });

  it("defaults to showing the language bar and harness badges", () => {
    const prefs = get(interfaceStore);
    expect(prefs.showLanguageBar).toBe(true);
    expect(prefs.showHarnessBadges).toBe(true);
  });

  it("status icon is opt-in and resets with interface preferences", () => {
    expect(get(interfaceStore).showStatusIcon).toBe(false);
    interfaceStore.setShowStatusIcon(true);
    expect(get(interfaceStore).showStatusIcon).toBe(true);
    expect(get(interfaceStore).hideDockWhenClosed).toBe(true);
    expect(get(interfaceStore).statusIconCounts).toBe(false);
    interfaceStore.setHideDockWhenClosed(false);
    interfaceStore.setStatusIconCounts(true);
    expect(get(interfaceStore).hideDockWhenClosed).toBe(false);
    expect(get(interfaceStore).statusIconCounts).toBe(true);
    interfaceStore.reset();
    expect(get(interfaceStore).showStatusIcon).toBe(false);
    expect(get(interfaceStore).hideDockWhenClosed).toBe(true);
    expect(get(interfaceStore).statusIconCounts).toBe(false);
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

  it("checks GitHub alerts on launch by default", () => {
    interfaceStore.reset();
    expect(get(interfaceStore).autoScanGithubAlerts).toBe(true);
    interfaceStore.setAutoScanGithubAlerts(false);
    expect(get(interfaceStore).autoScanGithubAlerts).toBe(false);
    interfaceStore.reset();
    expect(get(interfaceStore).autoScanGithubAlerts).toBe(true);
  });

  it("starts with the Fleet dashboard closed", () => {
    interfaceStore.reset();
    expect(get(interfaceStore).globalSurface).toBe("repository");
  });

  it("opens, closes and toggles the Fleet dashboard", () => {
    interfaceStore.reset();
    interfaceStore.setFleetOpen(true);
    expect(get(interfaceStore).globalSurface).toBe("fleet");
    interfaceStore.toggleFleet();
    expect(get(interfaceStore).globalSurface).toBe("repository");
    interfaceStore.toggleFleet();
    expect(get(interfaceStore).globalSurface).toBe("fleet");
    interfaceStore.setFleetOpen(false);
    expect(get(interfaceStore).globalSurface).toBe("repository");
  });

  it("opens and closes the Tasks surface without toggling", () => {
    interfaceStore.reset();
    interfaceStore.setTasksOpen(true);
    expect(get(interfaceStore).globalSurface).toBe("tasks");
    interfaceStore.setTasksOpen(true);
    expect(get(interfaceStore).globalSurface).toBe("tasks");
    interfaceStore.setTasksOpen(false);
    expect(get(interfaceStore).globalSurface).toBe("repository");
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
      expect(get(reloaded).globalSurface).toBe("fleet");
    } finally {
      if (restore) Object.defineProperty(globalThis, "window", restore);
      else Reflect.deleteProperty(globalThis, "window");
      vi.resetModules();
    }
  });

  it("opens Fleet Pulse by default, and remembers being closed", () => {
    // The panel is the answer to "what changed", so it opens; a reader who
    // shuts it has said they want the grid, and must not get it back tomorrow.
    expect(get(interfaceStore).fleetPulseOpen).toBe(true);
    interfaceStore.toggleFleetPulse();
    expect(get(interfaceStore).fleetPulseOpen).toBe(false);
  });

  it("shows every column until one is hidden", () => {
    expect(get(interfaceStore).fleetHiddenColumns).toEqual([]);
    interfaceStore.toggleFleetColumn("storage");
    expect(get(interfaceStore).fleetHiddenColumns).toEqual(["storage"]);
    interfaceStore.toggleFleetColumn("health");
    expect(get(interfaceStore).fleetHiddenColumns).toEqual(["storage", "health"]);
    interfaceStore.toggleFleetColumn("storage");
    expect(get(interfaceStore).fleetHiddenColumns).toEqual(["health"]);
  });

  it("brings everything back in one action", () => {
    // Hiding columns one at a time and having to unhide them one at a time is
    // how a reader ends up with a grid missing a column they forgot about.
    for (const key of ["storage", "health", "coverage"]) interfaceStore.toggleFleetColumn(key);
    interfaceStore.showAllFleetColumns();
    expect(get(interfaceStore).fleetHiddenColumns).toEqual([]);
  });

  it("toggles density without touching anything else", () => {
    interfaceStore.toggleFleetColumn("storage");
    interfaceStore.toggleFleetCompact();
    expect(get(interfaceStore).fleetCompact).toBe(true);
    expect(get(interfaceStore).fleetHiddenColumns).toEqual(["storage"]);
    interfaceStore.toggleFleetCompact();
    expect(get(interfaceStore).fleetCompact).toBe(false);
  });

  it("refuses an unbounded or non-string column list from storage", async () => {
    // A corrupt blob must not be able to grow the hidden set without limit, or
    // put non-strings where the grid will look keys up.
    const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
    try {
      const storage = memoryStorage({
        gitpulse_interface_prefs: JSON.stringify({
          fleetHiddenColumns: [
            ...Array.from({ length: 500 }, (_, i) => `col-${i}`),
            42,
            null,
            "",
            "storage",
            "storage",
          ],
        }),
      });
      Object.defineProperty(globalThis, "window", {
        value: { localStorage: storage },
        configurable: true,
        writable: true,
      });
      vi.resetModules();
      const reloaded = (await import("../interfaceStore")).interfaceStore;
      const hidden = get(reloaded).fleetHiddenColumns;
      expect(hidden.length).toBeLessThanOrEqual(32);
      expect(hidden.every((key) => typeof key === "string" && key !== "")).toBe(true);
      expect(new Set(hidden).size).toBe(hidden.length);
    } finally {
      if (restore) Object.defineProperty(globalThis, "window", restore);
      else Reflect.deleteProperty(globalThis, "window");
      vi.resetModules();
    }
  });

  it("honours an explicit stored false for GitHub launch scans", async () => {
    const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
    const load = async (stored: unknown) => {
      const storage = memoryStorage({
        gitpulse_interface_prefs: JSON.stringify({ autoScanGithubAlerts: stored }),
      });
      Object.defineProperty(globalThis, "window", {
        value: { localStorage: storage },
        configurable: true,
        writable: true,
      });
      vi.resetModules();
      return get((await import("../interfaceStore")).interfaceStore).autoScanGithubAlerts;
    };
    try {
      expect(await load(false)).toBe(false);
      expect(await load(true)).toBe(true);
      // Missing or corrupt values follow the default (on), not coverage's
      // fail-closed-off rule: turning this off is opt-out, not opt-in.
      expect(await load(undefined)).toBe(true);
      expect(await load("no")).toBe(true);
    } finally {
      if (restore) Object.defineProperty(globalThis, "window", restore);
      else delete (globalThis as { window?: unknown }).window;
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
    interfaceStore.dismissUpdateVersion("1.2.3");
    expect(get(interfaceStore).dismissedUpdateVersion).toBe("1.2.3");

    interfaceStore.setCheckForUpdates(false);
    expect(get(interfaceStore).dismissedUpdateVersion).toBe("");
  });

  it("keeps a dismissal across an unrelated toggle-on", () => {
    interfaceStore.setCheckForUpdates(true);
    interfaceStore.dismissUpdateVersion("1.2.3");
    interfaceStore.setCheckForUpdates(true);
    expect(get(interfaceStore).dismissedUpdateVersion).toBe("1.2.3");
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

describe("interfaceStore display preferences", () => {
  beforeEach(() => {
    interfaceStore.reset();
  });

  it("ships the accent, motion, timestamp, diff and tab defaults", () => {
    // Every one of these is additive: a fresh install must render exactly
    // what it rendered before the settings existed.
    expect(get(interfaceStore)).toMatchObject({
      accent: "blue",
      reduceMotion: false,
      timestampStyle: "relative",
      diffLayout: "unified",
      diffWordWrap: false,
      diffSyntaxHighlight: true,
      diffIgnoreWhitespace: false,
      tabWidth: 8,
    });
  });

  it("sets each one without disturbing its neighbours", () => {
    interfaceStore.setAccent("teal");
    interfaceStore.setReduceMotion(true);
    interfaceStore.setTimestampStyle("absolute");
    interfaceStore.setDiffLayout("split");
    interfaceStore.setDiffWordWrap(true);
    interfaceStore.setDiffSyntaxHighlight(false);
    interfaceStore.setDiffIgnoreWhitespace(true);
    interfaceStore.setTabWidth(4);

    expect(get(interfaceStore)).toMatchObject({
      accent: "teal",
      reduceMotion: true,
      timestampStyle: "absolute",
      diffLayout: "split",
      diffWordWrap: true,
      diffSyntaxHighlight: false,
      diffIgnoreWhitespace: true,
      tabWidth: 4,
      // Untouched by any of the above.
      showLanguageBar: true,
      graphWidthMode: "balanced",
    });
  });

  it("restores every one of them on reset", () => {
    // "Restore defaults" has to mean the whole page, not the panel that
    // happened to be open when the button was added.
    interfaceStore.setAccent("rose");
    interfaceStore.setReduceMotion(true);
    interfaceStore.setTimestampStyle("absolute");
    interfaceStore.setDiffLayout("split");
    interfaceStore.setDiffWordWrap(true);
    interfaceStore.setDiffSyntaxHighlight(false);
    interfaceStore.setDiffIgnoreWhitespace(true);
    interfaceStore.setTabWidth(2);
    interfaceStore.reset();

    expect(get(interfaceStore)).toMatchObject({
      accent: "blue",
      reduceMotion: false,
      timestampStyle: "relative",
      diffLayout: "unified",
      diffWordWrap: false,
      diffSyntaxHighlight: true,
      diffIgnoreWhitespace: false,
      tabWidth: 8,
    });
  });

  it("falls back rather than trusting a corrupt display blob", async () => {
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
        accent: "chartreuse",
        reduceMotion: "yes",
        timestampStyle: "iso",
        diffLayout: "side-by-side",
        diffWordWrap: 1,
        diffSyntaxHighlight: null,
        diffIgnoreWhitespace: "true",
        tabWidth: 37,
      });
      expect(bad).toMatchObject({
        accent: "blue",
        reduceMotion: false,
        timestampStyle: "relative",
        diffLayout: "unified",
        diffWordWrap: false,
        diffSyntaxHighlight: true,
        diffIgnoreWhitespace: false,
        tabWidth: 8,
      });

      // The control: valid values do survive, so the cases above are not
      // passing because the reader ignores these fields.
      const good = await load({
        accent: "violet",
        reduceMotion: true,
        timestampStyle: "absolute",
        diffLayout: "split",
        diffWordWrap: true,
        diffSyntaxHighlight: false,
        diffIgnoreWhitespace: true,
        tabWidth: 2,
      });
      expect(good).toMatchObject({
        accent: "violet",
        reduceMotion: true,
        timestampStyle: "absolute",
        diffLayout: "split",
        diffWordWrap: true,
        diffSyntaxHighlight: false,
        diffIgnoreWhitespace: true,
        tabWidth: 2,
      });
    } finally {
      if (restore) Object.defineProperty(globalThis, "window", restore);
      else delete (globalThis as { window?: unknown }).window;
      vi.resetModules();
    }
  });
});

it("persists bounded terminal preferences through the existing store", () => {
  interfaceStore.reset();
  interfaceStore.setTerminalFontSize(1000);
  interfaceStore.setTerminalLauncher("codex");
  expect(get(interfaceStore).terminalFontSize).toBe(24);
  expect(get(interfaceStore).terminalLauncher).toBe("codex");
  interfaceStore.setTerminalFontSize(Number.NaN);
  expect(get(interfaceStore).terminalFontSize).toBe(12);
  interfaceStore.reset();
  expect(get(interfaceStore).terminalLauncher).toBe("shell");
});

it("reloads terminal preferences and rejects corrupt stored values", async () => {
  const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
  const storage = memoryStorage({ gitpulse_interface_prefs: JSON.stringify({ terminalFontSize: 20, terminalLauncher: "manvi" }) });
  try {
    Object.defineProperty(globalThis, "window", { value: {localStorage:storage}, configurable:true, writable:true });
    vi.resetModules();
    const first = (await import("../interfaceStore")).interfaceStore;
    expect(get(first)).toMatchObject({ terminalFontSize:20, terminalLauncher:"manvi" });
    first.setTerminalFontSize(18); first.setTerminalLauncher("codex");
    vi.resetModules();
    expect(get((await import("../interfaceStore")).interfaceStore)).toMatchObject({ terminalFontSize:18, terminalLauncher:"codex" });
    storage.setItem("gitpulse_interface_prefs", JSON.stringify({terminalFontSize:"large", terminalLauncher:"unknown"}));
    vi.resetModules();
    expect(get((await import("../interfaceStore")).interfaceStore)).toMatchObject({terminalFontSize:12, terminalLauncher:"shell"});
  } finally {
    if (restore) Object.defineProperty(globalThis, "window", restore);
    else Reflect.deleteProperty(globalThis, "window");
    vi.resetModules();
  }
});
