/**
 * Pure state, bounds, and routing rules for the docked File Explorer sidebar
 * inside `TerminalPanel.svelte`.
 *
 * Kept separate from `splitTree.ts` on purpose: toggling the terminal's File
 * Explorer sidebar docks an explorer rail beside the pane canvas without
 * adding a 3rd split leaf or closing tabs. Where a clicked file opens is not
 * stored here: it is the app-wide setting (`sidebarNavStore.openTarget`),
 * decided by `resolveFileOpen`, so both explorers answer to one choice.
 */

import { writable, type Readable } from "svelte/store";

export const TERMINAL_EXPLORER_WIDTH_DEFAULT = 248;
export const TERMINAL_EXPLORER_WIDTH_MIN = 180;
export const TERMINAL_EXPLORER_WIDTH_MAX = 420;
export const EXPLORER_SIDEBAR_STORAGE_KEY = "gitpulse.terminal.explorerSidebar";

export interface ExplorerSidebarPrefs {
  readonly open: boolean;
  readonly width: number;
}

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export function clampExplorerSidebarWidth(width: number): number {
  if (!Number.isFinite(width)) return TERMINAL_EXPLORER_WIDTH_DEFAULT;
  return Math.min(
    TERMINAL_EXPLORER_WIDTH_MAX,
    Math.max(TERMINAL_EXPLORER_WIDTH_MIN, Math.round(width)),
  );
}

export function readExplorerSidebarPrefs(
  storage: Pick<StorageLike, "getItem"> | null | undefined = globalThis.localStorage,
): ExplorerSidebarPrefs {
  const defaults: ExplorerSidebarPrefs = {
    open: false,
    width: TERMINAL_EXPLORER_WIDTH_DEFAULT,
  };
  if (!storage || typeof storage.getItem !== "function") return defaults;
  try {
    const raw = storage.getItem(EXPLORER_SIDEBAR_STORAGE_KEY);
    if (!raw) return defaults;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return defaults;
    }
    const rec = parsed as Record<string, unknown>;
    return {
      open: rec.open === true,
      width:
        typeof rec.width === "number"
          ? clampExplorerSidebarWidth(rec.width)
          : TERMINAL_EXPLORER_WIDTH_DEFAULT,
    };
  } catch {
    return defaults;
  }
}

export function writeExplorerSidebarPrefs(
  prefs: ExplorerSidebarPrefs,
  storage: Pick<StorageLike, "setItem"> | null | undefined = globalThis.localStorage,
): void {
  if (!storage || typeof storage.setItem !== "function") return;
  try {
    const sanitized: ExplorerSidebarPrefs = {
      open: Boolean(prefs.open),
      width: clampExplorerSidebarWidth(prefs.width),
    };
    storage.setItem(EXPLORER_SIDEBAR_STORAGE_KEY, JSON.stringify(sanitized));
  } catch {
    /* localStorage unavailable or quota exceeded */
  }
}

export interface TerminalExplorerSidebarStore extends Readable<ExplorerSidebarPrefs> {
  setOpen(open: boolean): void;
  toggle(): void;
  setWidthLive(width: number): void;
  setWidth(width: number): void;
}

export function createTerminalExplorerSidebarStore(
  storage: StorageLike | null | undefined = globalThis.localStorage,
): TerminalExplorerSidebarStore {
  const { subscribe, update } = writable<ExplorerSidebarPrefs>(readExplorerSidebarPrefs(storage));

  function commit(next: ExplorerSidebarPrefs): ExplorerSidebarPrefs {
    writeExplorerSidebarPrefs(next, storage);
    return next;
  }

  return {
    subscribe,
    setOpen: (open) =>
      update((current) => {
        const nextOpen = Boolean(open);
        return current.open === nextOpen ? current : commit({ ...current, open: nextOpen });
      }),
    toggle: () => update((current) => commit({ ...current, open: !current.open })),
    setWidthLive: (width) =>
      update((current) => ({ ...current, width: clampExplorerSidebarWidth(width) })),
    setWidth: (width) =>
      update((current) => commit({ ...current, width: clampExplorerSidebarWidth(width) })),
  };
}

export const terminalExplorerSidebar = createTerminalExplorerSidebarStore();
