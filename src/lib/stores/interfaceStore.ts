import { writable } from "svelte/store";
import {
  isGraphWidthMode,
  type GraphWidthMode,
} from "../graph/graphLayout";

export interface InterfacePrefs {
  showLanguageBar: boolean;
  showHarnessBadges: boolean;
  /** Author-avatar column in the commit graph gutter. */
  showGraphAvatars: boolean;
  /** Maximum share of the graph view used by the lane viewport. */
  graphWidthMode: GraphWidthMode;
  /** Global UI font / zoom scale factor (e.g. 1.0 = 100%, 0.9 = 90%, 1.1 = 110%). */
  uiFontScale: number;
  /** Map of dismissed coach mark IDs. */
  seenCoachMarks: Record<string, boolean>;
  /**
   * Opt-in automatic release check. Off by default: GitPulse makes no network
   * request about itself until the user asks for one.
   */
  checkForUpdates: boolean;
  /** Epoch ms of the last completed automatic check; 0 means never. */
  lastUpdateCheckAt: number;
  /** Version whose update notice the user dismissed ("" means none). */
  dismissedUpdateVersion: string;
}

const STORAGE_KEY = "gitpulse_interface_prefs";

const DEFAULTS: InterfacePrefs = {
  showLanguageBar: true,
  showHarnessBadges: true,
  showGraphAvatars: true,
  graphWidthMode: "balanced",
  uiFontScale: 1.0,
  seenCoachMarks: {},
  checkForUpdates: false,
  lastUpdateCheckAt: 0,
  dismissedUpdateVersion: "",
};

function readPrefs(): InterfacePrefs {
  if (typeof window === "undefined" || !window.localStorage) return { ...DEFAULTS };
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULTS };
    const parsed = JSON.parse(raw) as Partial<InterfacePrefs>;
    return {
      showLanguageBar:
        typeof parsed.showLanguageBar === "boolean"
          ? parsed.showLanguageBar
          : DEFAULTS.showLanguageBar,
      showHarnessBadges:
        typeof parsed.showHarnessBadges === "boolean"
          ? parsed.showHarnessBadges
          : DEFAULTS.showHarnessBadges,
      showGraphAvatars:
        typeof parsed.showGraphAvatars === "boolean"
          ? parsed.showGraphAvatars
          : DEFAULTS.showGraphAvatars,
      graphWidthMode: isGraphWidthMode(parsed.graphWidthMode)
        ? parsed.graphWidthMode
        : DEFAULTS.graphWidthMode,
      uiFontScale:
        typeof parsed.uiFontScale === "number" &&
        parsed.uiFontScale >= 0.75 &&
        parsed.uiFontScale <= 1.5
          ? parsed.uiFontScale
          : DEFAULTS.uiFontScale,
      seenCoachMarks:
        parsed.seenCoachMarks && typeof parsed.seenCoachMarks === "object"
          ? parsed.seenCoachMarks
          : {},
      // Anything other than an explicit `true` leaves the check off. A
      // corrupt or partially-written value must never opt a user in.
      checkForUpdates: parsed.checkForUpdates === true,
      lastUpdateCheckAt:
        typeof parsed.lastUpdateCheckAt === "number" &&
        Number.isFinite(parsed.lastUpdateCheckAt) &&
        parsed.lastUpdateCheckAt >= 0
          ? parsed.lastUpdateCheckAt
          : DEFAULTS.lastUpdateCheckAt,
      dismissedUpdateVersion:
        typeof parsed.dismissedUpdateVersion === "string"
          ? parsed.dismissedUpdateVersion
          : DEFAULTS.dismissedUpdateVersion,
    };
  } catch {
    /* corrupt or unavailable storage falls back to defaults */
    return { ...DEFAULTS };
  }
}

function persist(prefs: InterfacePrefs) {
  if (typeof window === "undefined" || !window.localStorage) return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    /* ignore quota / private-mode failures */
  }
}

function createInterfaceStore() {
  const initial = readPrefs();
  const { subscribe, set, update } = writable<InterfacePrefs>(initial);

  function commit(next: InterfacePrefs) {
    persist(next);
    set(next);
  }

  return {
    subscribe,
    setShowLanguageBar: (show: boolean) =>
      update((prefs) => {
        const next = { ...prefs, showLanguageBar: show };
        persist(next);
        return next;
      }),
    setShowHarnessBadges: (show: boolean) =>
      update((prefs) => {
        const next = { ...prefs, showHarnessBadges: show };
        persist(next);
        return next;
      }),
    setShowGraphAvatars: (show: boolean) =>
      update((prefs) => {
        const next = { ...prefs, showGraphAvatars: show };
        persist(next);
        return next;
      }),
    setGraphWidthMode: (mode: GraphWidthMode) =>
      update((prefs) => {
        const next = { ...prefs, graphWidthMode: mode };
        persist(next);
        return next;
      }),
    toggleGraphAvatars: () =>
      update((prefs) => {
        const next = { ...prefs, showGraphAvatars: !prefs.showGraphAvatars };
        persist(next);
        return next;
      }),
    setFontScale: (scale: number) =>
      update((prefs) => {
        const clamped = Math.round(Math.max(0.75, Math.min(1.5, scale)) * 100) / 100;
        const next = { ...prefs, uiFontScale: clamped };
        persist(next);
        return next;
      }),
    zoomIn: () =>
      update((prefs) => {
        const nextScale = Math.round(Math.min(1.5, prefs.uiFontScale + 0.05) * 100) / 100;
        const next = { ...prefs, uiFontScale: nextScale };
        persist(next);
        return next;
      }),
    zoomOut: () =>
      update((prefs) => {
        const nextScale = Math.round(Math.max(0.75, prefs.uiFontScale - 0.05) * 100) / 100;
        const next = { ...prefs, uiFontScale: nextScale };
        persist(next);
        return next;
      }),
    resetZoom: () =>
      update((prefs) => {
        const next = { ...prefs, uiFontScale: 1.0 };
        persist(next);
        return next;
      }),
    setCheckForUpdates: (enabled: boolean) =>
      update((prefs) => {
        // Turning the check off also clears the dismissal, so re-enabling it
        // later reports honestly instead of staying silent about a version
        // the user dismissed under different settings.
        const next = {
          ...prefs,
          checkForUpdates: enabled,
          dismissedUpdateVersion: enabled ? prefs.dismissedUpdateVersion : "",
        };
        persist(next);
        return next;
      }),
    /** Records that an automatic check completed, restarting the throttle. */
    markUpdateChecked: (at: number) =>
      update((prefs) => {
        const next = { ...prefs, lastUpdateCheckAt: at };
        persist(next);
        return next;
      }),
    /** Silences the notice for one specific version only. */
    dismissUpdateVersion: (version: string) =>
      update((prefs) => {
        const next = { ...prefs, dismissedUpdateVersion: version };
        persist(next);
        return next;
      }),
    dismissCoachMark: (id: string) =>
      update((prefs) => {
        const next = {
          ...prefs,
          seenCoachMarks: { ...prefs.seenCoachMarks, [id]: true },
        };
        persist(next);
        return next;
      }),
    resetCoachMarks: () =>
      update((prefs) => {
        const next = { ...prefs, seenCoachMarks: {} };
        persist(next);
        return next;
      }),
    reset: () => commit({ ...DEFAULTS }),
  };
}

export const interfaceStore = createInterfaceStore();
