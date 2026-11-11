import { writable, type Readable } from "svelte/store";

/**
 * How the sidebar and the terminal dock take their room (DESIGN_LANGUAGE §8.5).
 *
 * `docked` is the layout the app has always had: the panel owns a column or a
 * row, the workspace reflows around it, and a toggle animates the size once.
 * `overlay` keeps the workspace at its collapsed layout and floats the open
 * panel above it, sliding on the compositor (transform and opacity only).
 * Which one is the default is the owner's decision (§14, decision 1); it is
 * `docked` until that is made, so no one's layout changes under them.
 */
export type PanelMode = "docked" | "overlay";

export interface PanelModes {
  sidebar: PanelMode;
  dock: PanelMode;
}

export const PANEL_MODE_STORAGE_KEY = "gitpulse_panel_modes";

export const DEFAULT_PANEL_MODES: Readonly<PanelModes> = Object.freeze({
  sidebar: "docked",
  dock: "docked",
});

export function isPanelMode(value: unknown): value is PanelMode {
  return value === "docked" || value === "overlay";
}

/** Field-wise defensive parse: one bad field falls back alone. */
export function parsePanelModes(raw: string | null): PanelModes {
  if (!raw) return { ...DEFAULT_PANEL_MODES };
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw) as unknown;
  } catch {
    return { ...DEFAULT_PANEL_MODES };
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { ...DEFAULT_PANEL_MODES };
  }
  const record = parsed as Record<string, unknown>;
  return {
    sidebar: isPanelMode(record.sidebar) ? record.sidebar : DEFAULT_PANEL_MODES.sidebar,
    dock: isPanelMode(record.dock) ? record.dock : DEFAULT_PANEL_MODES.dock,
  };
}

function readStorage(): Storage | null {
  try {
    if (typeof window === "undefined" || !window.localStorage) return null;
    return window.localStorage;
  } catch {
    return null;
  }
}

export interface PanelModeStore extends Readable<PanelModes> {
  set(panel: keyof PanelModes, mode: PanelMode): void;
  toggle(panel: keyof PanelModes): void;
}

export function createPanelModeStore(storage: Pick<Storage, "getItem" | "setItem"> | null = readStorage()): PanelModeStore {
  const { subscribe, update } = writable<PanelModes>(parsePanelModes(storage?.getItem(PANEL_MODE_STORAGE_KEY) ?? null));

  function commit(next: PanelModes): PanelModes {
    try {
      storage?.setItem(PANEL_MODE_STORAGE_KEY, JSON.stringify(next));
    } catch {
      /* quota or private mode: keep the in-memory choice */
    }
    return next;
  }

  return {
    subscribe,
    set: (panel, mode) => update((current) => (current[panel] === mode ? current : commit({ ...current, [panel]: mode }))),
    toggle: (panel) =>
      update((current) => commit({ ...current, [panel]: current[panel] === "docked" ? "overlay" : "docked" })),
  };
}

export const panelModes = createPanelModeStore();
