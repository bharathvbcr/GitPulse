import { writable } from "svelte/store";

export type Theme = "dark" | "light";
export type ThemePreference = "system" | Theme;

const STORAGE_KEY = "gitpulse_theme_preference";

function systemTheme(): Theme {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return "dark";
  }
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function readPreference(): ThemePreference {
  if (typeof localStorage === "undefined") return "system";
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === "light" || raw === "dark" || raw === "system") return raw;
  } catch {
    /* ignore quota / private-mode failures */
  }
  return "system";
}

function persist(preference: ThemePreference) {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(STORAGE_KEY, preference);
  } catch {
    /* ignore quota / private-mode failures */
  }
}

function applyResolved(theme: Theme) {
  if (typeof document === "undefined") return;
  document.documentElement.classList.toggle("dark", theme === "dark");
  document.documentElement.classList.toggle("light", theme === "light");
}

type ViewTransitionDocument = Document & {
  startViewTransition?: (update: () => void) => unknown;
};

/**
 * Crossfade the whole window when the webview supports the View Transitions
 * API (Safari/WebKit 18+); older runtimes just flip instantly. The canvas
 * repaint happens inside the transition callback, so nothing animates twice.
 */
function applyWithTransition(theme: Theme) {
  if (typeof document === "undefined") return;
  const doc = document as ViewTransitionDocument;
  const reduceMotion =
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (typeof doc.startViewTransition !== "function" || reduceMotion) {
    applyResolved(theme);
    return;
  }
  doc.startViewTransition(() => applyResolved(theme));
}

export function createThemeStore() {
  let preference = readPreference();
  const initial = preference === "system" ? systemTheme() : preference;
  // First paint applies directly — there is nothing to crossfade from.
  applyResolved(initial);
  const { subscribe, set } = writable<Theme>(initial);

  /** Re-renders the runtime theme without touching stored preference state. */
  function resolveTo(resolved: Theme) {
    applyWithTransition(resolved);
    set(resolved);
  }

  function apply(nextPreference: ThemePreference) {
    preference = nextPreference;
    // Only an explicit user selection is durable; OS flips re-resolve at
    // runtime through the listener below instead of rewriting "system" on
    // every event.
    persist(preference);
    resolveTo(preference === "system" ? systemTheme() : preference);
  }

  // A module-singleton store lives as long as the webview, so this listener
  // is never torn down: there is no second consumer to leak into and no
  // lifetime that outlives the document it watches.
  if (typeof window !== "undefined" && typeof window.matchMedia === "function") {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => {
      if (preference === "system") resolveTo(systemTheme());
    };
    if (typeof media.addEventListener === "function") {
      media.addEventListener("change", onChange);
    } else if (typeof media.addListener === "function") {
      media.addListener(onChange);
    }
  }

  return {
    subscribe,
    toggle: () => {
      const current = preference === "system" ? systemTheme() : preference;
      apply(current === "dark" ? "light" : "dark");
    },
    setTheme: (theme: Theme) => apply(theme),
    setPreference: (next: ThemePreference) => apply(next),
    preference: () => preference,
  };
}

export const themeStore = createThemeStore();
