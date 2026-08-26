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
}

const STORAGE_KEY = "gitpulse_interface_prefs";

const DEFAULTS: InterfacePrefs = {
  showLanguageBar: true,
  showHarnessBadges: true,
  showGraphAvatars: true,
  graphWidthMode: "balanced",
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
    reset: () => commit({ ...DEFAULTS }),
  };
}

export const interfaceStore = createInterfaceStore();
