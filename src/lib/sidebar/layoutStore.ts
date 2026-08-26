import { writable, type Readable } from "svelte/store";
import {
  SIDEBAR_DEFAULT_WIDTH,
  clampSidebarWidth,
} from "./metrics";

export const STORAGE_KEY = "gitpulse_sidebar_layout";
export const SECTION_STORAGE_KEY = "gitpulse_sidebar_sections";

export interface SidebarLayout {
  width: number;
  collapsed: boolean;
}

/**
 * Collapsed-section flags for the staged/unstaged headers. `true` means the
 * section is collapsed (chevron points right); both default to open.
 */
export interface SidebarSections {
  staged: boolean;
  unstaged: boolean;
}

const DEFAULT_LAYOUT: SidebarLayout = {
  width: SIDEBAR_DEFAULT_WIDTH,
  collapsed: false,
};

const DEFAULT_SECTIONS: SidebarSections = {
  staged: false,
  unstaged: false,
};

function readStorage(): Storage | null {
  try {
    if (typeof window === "undefined" || !window.localStorage) return null;
    return window.localStorage;
  } catch {
    /* private-mode / disabled storage — run in-memory only */
    return null;
  }
}

/**
 * Pure parse of a persisted layout blob. Field-wise defensive: a valid width
 * survives a garbage `collapsed` and vice versa; anything unusable falls back
 * per-field to defaults. Widths always pass through clampSidebarWidth so an
 * out-of-range or non-finite number can never poison the shell.
 */
export function loadLayout(raw: string | null): SidebarLayout {
  if (!raw) return { ...DEFAULT_LAYOUT };
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw) as unknown;
  } catch {
    return { ...DEFAULT_LAYOUT };
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { ...DEFAULT_LAYOUT };
  }
  const record = parsed as Record<string, unknown>;
  return {
    width:
      typeof record.width === "number"
        ? clampSidebarWidth(record.width)
        : DEFAULT_LAYOUT.width,
    collapsed:
      typeof record.collapsed === "boolean"
        ? record.collapsed
        : DEFAULT_LAYOUT.collapsed,
  };
}

export function saveLayout(layout: SidebarLayout): void {
  const storage = readStorage();
  if (!storage) return;
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(layout));
  } catch {
    /* quota / private mode — fail closed, keep in-memory state */
  }
}

/** Pure parse of the persisted collapsed-section flags. */
export function loadSections(raw: string | null): SidebarSections {
  if (!raw) return { ...DEFAULT_SECTIONS };
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw) as unknown;
  } catch {
    return { ...DEFAULT_SECTIONS };
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { ...DEFAULT_SECTIONS };
  }
  const record = parsed as Record<string, unknown>;
  return {
    staged: typeof record.staged === "boolean" ? record.staged : DEFAULT_SECTIONS.staged,
    unstaged:
      typeof record.unstaged === "boolean"
        ? record.unstaged
        : DEFAULT_SECTIONS.unstaged,
  };
}

/**
 * Persists section flags. Pass an explicit storage in tests; production uses
 * window.localStorage and swallows quota/private-mode failures.
 */
export function saveSections(
  sections: SidebarSections,
  storage: Pick<Storage, "setItem"> | null = readStorage(),
): void {
  if (!storage) return;
  try {
    storage.setItem(SECTION_STORAGE_KEY, JSON.stringify(sections));
  } catch {
    /* quota / private mode — fail closed, keep in-memory state */
  }
}

export function readSectionsRaw(): string | null {
  return readStorage()?.getItem(SECTION_STORAGE_KEY) ?? null;
}

export interface LayoutStore extends Readable<SidebarLayout> {
  setWidth(px: number): void;
  /** Drag-stream variant: updates state without persisting. */
  setWidthLive(px: number): void;
  toggleCollapsed(): void;
  reset(): void;
}

export function createLayoutStore(): LayoutStore {
  const initial = loadLayout(readStorage()?.getItem(STORAGE_KEY) ?? null);
  const { subscribe, update } = writable<SidebarLayout>(initial);

  function commit(next: SidebarLayout) {
    saveLayout(next);
    return next;
  }

  return {
    subscribe,
    // Synchronous persistence IS the contract (tests pin it): every mutation
    // is user-deliberate. Drag streams bypass this via setWidthLive below —
    // the CALLER owns that policy because it owns the gesture lifecycle.
    setWidth: (px: number) => update((current) => commit({ ...current, width: clampSidebarWidth(px) })),
    /** In-memory-only width update for high-frequency drag streams; pair
     * with one final setWidth(current width) on gesture end to persist. */
    setWidthLive: (px: number) =>
      update((current) => ({ ...current, width: clampSidebarWidth(px) })),
    toggleCollapsed: () =>
      update((current) => commit({ ...current, collapsed: !current.collapsed })),
    reset: () => update(() => commit({ ...DEFAULT_LAYOUT })),
  };
}

export const layoutStore = createLayoutStore();
