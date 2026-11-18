/**
 * Pure state, bounds, and routing rules for the docked File Explorer sidebar
 * inside `TerminalPanel.svelte`.
 *
 * Kept separate from `splitTree.ts` on purpose: toggling the terminal's File
 * Explorer sidebar docks an explorer rail beside the pane canvas without
 * adding a 3rd split leaf or closing tabs, and clicking a file follows the
 * user's chosen open target (`tab` | `split` | `code`) rather than guessing.
 */

import { writable, type Readable } from "svelte/store";
import { resolveSidebarFileAction } from "../sidebar/layoutStore";

export const TERMINAL_EXPLORER_WIDTH_DEFAULT = 248;
export const TERMINAL_EXPLORER_WIDTH_MIN = 180;
export const TERMINAL_EXPLORER_WIDTH_MAX = 420;
export const EXPLORER_SIDEBAR_STORAGE_KEY = "gitpulse.terminal.explorerSidebar";

export type ExplorerOpenTarget = "tab" | "split" | "code";

export type ResolvedExplorerFileAction =
  | "update-visible-pane"
  | "open-tab"
  | "open-split"
  | "open-code";

export interface ExplorerSidebarPrefs {
  readonly open: boolean;
  readonly target: ExplorerOpenTarget;
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

export function normalizeExplorerOpenTarget(raw: unknown): ExplorerOpenTarget {
  if (raw === "split" || raw === "code" || raw === "tab") return raw;
  return "tab";
}

/**
 * Resolves what clicking a file in the terminal's docked File Explorer sidebar
 * should do. Which surface opens is the app sidebar explorer's rule
 * (`resolveSidebarFileAction`: `Alt+Click` -> split, `⌘/Ctrl+Click` -> the
 * surface the default is not, else the user's target), so a modifier click
 * means the same thing in both explorers. This adds only the terminal's own
 * step: a tab-target click retargets a file pane already on screen rather
 * than opening another.
 */
export function resolveExplorerFileAction(options: {
  defaultTarget: ExplorerOpenTarget;
  altKey?: boolean;
  metaOrCtrlKey?: boolean;
  hasVisibleFilePane: boolean;
}): ResolvedExplorerFileAction {
  const target = resolveSidebarFileAction({
    defaultTarget: options.defaultTarget,
    altKey: options.altKey,
    metaOrCtrlKey: options.metaOrCtrlKey,
  });
  if (target === "code") return "open-code";
  if (target === "split") return "open-split";
  return options.hasVisibleFilePane ? "update-visible-pane" : "open-tab";
}

export function readExplorerSidebarPrefs(
  storage: Pick<StorageLike, "getItem"> | null | undefined = globalThis.localStorage,
): ExplorerSidebarPrefs {
  const defaults: ExplorerSidebarPrefs = {
    open: false,
    target: "tab",
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
      target: normalizeExplorerOpenTarget(rec.target),
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
      target: normalizeExplorerOpenTarget(prefs.target),
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
  setTarget(target: ExplorerOpenTarget): void;
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
    setTarget: (target) =>
      update((current) => {
        const nextTarget = normalizeExplorerOpenTarget(target);
        return current.target === nextTarget ? current : commit({ ...current, target: nextTarget });
      }),
    setWidthLive: (width) =>
      update((current) => ({ ...current, width: clampExplorerSidebarWidth(width) })),
    setWidth: (width) =>
      update((current) => commit({ ...current, width: clampExplorerSidebarWidth(width) })),
  };
}

export const terminalExplorerSidebar = createTerminalExplorerSidebarStore();
