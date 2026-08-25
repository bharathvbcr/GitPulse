import { writable } from "svelte/store";

export interface InterfacePrefs {
  showLanguageBar: boolean;
  showHarnessBadges: boolean;
}

const STORAGE_KEY = "gitpulse_interface_prefs";

const DEFAULTS: InterfacePrefs = {
  showLanguageBar: true,
  showHarnessBadges: true,
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
    reset: () => commit({ ...DEFAULTS }),
  };
}

export const interfaceStore = createInterfaceStore();
