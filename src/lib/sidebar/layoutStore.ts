import { writable, type Readable } from "svelte/store";
import { handleTablistKeydown } from "../dom/tablist";
import {
  SIDEBAR_DEFAULT_WIDTH,
  clampSidebarWidth,
} from "./metrics";

export const STORAGE_KEY = "gitpulse_sidebar_layout";
export const SECTION_STORAGE_KEY = "gitpulse_sidebar_sections";
export const SIDEBAR_NAV_STORAGE_KEY = "gitpulse_sidebar_nav";

export type SidebarView = "git" | "files";
/**
 * Where a clicked file opens. "auto" is the surface the user last worked in:
 * a terminal split while the terminal was, the Code page otherwise
 * (`resolveFileOpen`); the others pin it.
 */
export type SidebarFileOpenTarget = "auto" | "code" | "tab" | "split";

/** A target after "auto" has been decided: where a file actually opens. */
export type ResolvedFileOpen = Exclude<SidebarFileOpenTarget, "auto">;

export interface SidebarNavPrefs {
  view: SidebarView;
  openTarget: SidebarFileOpenTarget;
}

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

const DEFAULT_SIDEBAR_NAV: SidebarNavPrefs = {
  view: "git",
  openTarget: "auto",
};

/**
 * Version of the persisted nav blob. Unversioned blobs always carry an
 * `openTarget`, because every commit (a view switch included) wrote it, so a
 * stored "code" there could be the old default rather than a choice; version 2
 * is the first in which "code" is only ever chosen.
 */
const SIDEBAR_NAV_VERSION = 2;

export function normalizeSidebarView(raw: unknown): SidebarView {
  return raw === "files" ? "files" : "git";
}

/** The sidebar's views, in the order its switcher draws them. */
export const SIDEBAR_VIEWS: readonly SidebarView[] = ["git", "files"];

/**
 * The view an arrow, Home or End press in the Git | Explorer tablist moves to,
 * or null for a key the tablist does not own. The movement is the shared
 * tablist's (`dom/tablist.ts`), so it wraps and jumps like every other tab
 * strip here.
 */
export function sidebarViewForKey(current: SidebarView, key: string): SidebarView | null {
  const move = handleTablistKeydown(key, SIDEBAR_VIEWS.indexOf(current), SIDEBAR_VIEWS.length);
  return move ? (SIDEBAR_VIEWS[move.index] ?? null) : null;
}

export function normalizeSidebarOpenTarget(raw: unknown): SidebarFileOpenTarget {
  if (raw === "tab" || raw === "split" || raw === "code" || raw === "auto") return raw;
  return "auto";
}

/** How long after a click its double-click is taken to belong to it. */
export const SIDEBAR_DOUBLE_CLICK_WINDOW_MS = 1500;

/** Where an explorer's last click on a file sent it. */
export interface SidebarLastOpen {
  path: string;
  action: ResolvedFileOpen;
  at: number;
}

/**
 * Whether a double-click on `path` at `now` belongs to the last click, so it
 * follows where that click sent the file. One rule for every explorer that
 * remembers its last open: the main sidebar's and the terminal's docked one.
 */
export function doubleClickFollows(
  lastOpen: { path: string; at: number } | null,
  path: string,
  now: number,
): boolean {
  return (
    lastOpen !== null &&
    lastOpen.path === path &&
    now >= lastOpen.at &&
    now - lastOpen.at <= SIDEBAR_DOUBLE_CLICK_WINDOW_MS
  );
}

/**
 * The one rule for where a click or double-click on a file in either explorer
 * (the app sidebar's, the terminal dock's) opens it. In order:
 *
 * 1. A double-click (`pin`) arrives after its two clicks have already resolved
 *    (and consumed) the modifier keys, so it follows the target its own clicks
 *    chose instead of resolving again with none held. Otherwise an
 *    Alt-double-click, a split, would pin the file in Code and switch views.
 * 2. `Alt+Click` opens a terminal split.
 * 3. The setting decides the base: "auto" is a terminal split while the
 *    terminal is the active surface (`terminalActive`) and the Code page
 *    otherwise; "code", "tab" and "split" are what they say.
 * 4. `⌘/Ctrl+Click` swaps the base's side: Code becomes a terminal tab, any
 *    terminal target becomes Code. The swap applies to the decided base, so
 *    under "auto" it is never a no-op.
 *
 * A "tab" reuses an unkept file pane already on screen rather than adding
 * one; that is how the terminal runs it, not a decision made here.
 */
export function resolveFileOpen(options: {
  path: string;
  pin: boolean;
  lastOpen: SidebarLastOpen | null;
  now: number;
  defaultTarget: SidebarFileOpenTarget;
  /** Whether the terminal dock is open for this repository and was the last surface used. */
  terminalActive: boolean;
  altKey?: boolean;
  metaOrCtrlKey?: boolean;
}): ResolvedFileOpen {
  const { lastOpen } = options;
  if (options.pin && lastOpen && doubleClickFollows(lastOpen, options.path, options.now)) {
    return lastOpen.action;
  }
  if (options.altKey) return "split";
  const target = normalizeSidebarOpenTarget(options.defaultTarget);
  const base: ResolvedFileOpen = target === "auto" ? (options.terminalActive ? "split" : "code") : target;
  if (options.metaOrCtrlKey) return base === "code" ? "tab" : "code";
  return base;
}

/**
 * Pure parse of the persisted nav blob. Only a version-2 blob's "code" is a
 * choice (`SIDEBAR_NAV_VERSION`); an unversioned one keeps "tab" and "split"
 * and reads anything else, "code" included, as the "auto" default.
 */
export function loadSidebarNav(raw: string | null): SidebarNavPrefs {
  if (!raw) return { ...DEFAULT_SIDEBAR_NAV };
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw) as unknown;
  } catch {
    return { ...DEFAULT_SIDEBAR_NAV };
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { ...DEFAULT_SIDEBAR_NAV };
  }
  const record = parsed as Record<string, unknown>;
  const stored = normalizeSidebarOpenTarget(record.openTarget);
  const openTarget =
    record.v === SIDEBAR_NAV_VERSION || stored === "tab" || stored === "split" ? stored : "auto";
  return {
    view: normalizeSidebarView(record.view),
    openTarget,
  };
}

export function saveSidebarNav(
  prefs: SidebarNavPrefs,
  storage: Pick<Storage, "setItem"> | null = readStorage(),
): void {
  if (!storage) return;
  try {
    const sanitized = {
      v: SIDEBAR_NAV_VERSION,
      view: normalizeSidebarView(prefs.view),
      openTarget: normalizeSidebarOpenTarget(prefs.openTarget),
    };
    storage.setItem(SIDEBAR_NAV_STORAGE_KEY, JSON.stringify(sanitized));
  } catch {
    /* quota / private mode — fail closed, keep in-memory state */
  }
}

export interface SidebarNavStore extends Readable<SidebarNavPrefs> {
  setView(view: SidebarView): void;
  setOpenTarget(target: SidebarFileOpenTarget): void;
  reset(): void;
}

export function createSidebarNavStore(): SidebarNavStore {
  const initial = loadSidebarNav(readStorage()?.getItem(SIDEBAR_NAV_STORAGE_KEY) ?? null);
  const { subscribe, update } = writable<SidebarNavPrefs>(initial);

  function commit(next: SidebarNavPrefs): SidebarNavPrefs {
    saveSidebarNav(next);
    return next;
  }

  return {
    subscribe,
    setView: (view) =>
      update((current) => {
        const nextView = normalizeSidebarView(view);
        return current.view === nextView ? current : commit({ ...current, view: nextView });
      }),
    setOpenTarget: (openTarget) =>
      update((current) => {
        const nextTarget = normalizeSidebarOpenTarget(openTarget);
        return current.openTarget === nextTarget
          ? current
          : commit({ ...current, openTarget: nextTarget });
      }),
    reset: () => update(() => commit({ ...DEFAULT_SIDEBAR_NAV })),
  };
}

export const sidebarNavStore = createSidebarNavStore();

let currentSidebarExplorerDocked = false;
const sidebarExplorerDockedStore = writable(false);

/** True while a mounted, expanded, docked main Sidebar is already displaying ExplorerRail. */
export const sidebarExplorerDocked: Readable<boolean> = {
  subscribe: sidebarExplorerDockedStore.subscribe,
};

export function setSidebarExplorerDocked(active: boolean): void {
  const next = Boolean(active);
  if (currentSidebarExplorerDocked === next) return;
  currentSidebarExplorerDocked = next;
  sidebarExplorerDockedStore.set(next);
}
