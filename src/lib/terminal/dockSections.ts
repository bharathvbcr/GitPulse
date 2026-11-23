/**
 * The dock's sections: what the dock beneath the view is showing.
 *
 * The dock houses more than a shell — agents, files and the policy console
 * sit beside it — but the tabs themselves are never partitioned by kind: a
 * split pair deliberately mixes a shell and an agent, and handoff runs
 * between them. So Shell is the workspace (every tab, every split) and the
 * other three are lenses that hand back to it.
 *
 * Pure data and one small persisted store; `TerminalPanel.svelte` owns the
 * per-panel state and `DockHeader.svelte` draws the switcher.
 */

import { writable, type Readable } from "svelte/store";
import type { HostOS } from "../platform";
import { platformChord } from "../ui/platformCopy";

export type DockSection = "shell" | "agents" | "files" | "console";

export interface DockSectionInfo {
  readonly id: DockSection;
  readonly label: string;
  /** Tooltip copy: what the section shows. */
  readonly hint: string;
}

export const DOCK_SECTIONS: readonly DockSectionInfo[] = [
  { id: "shell", label: "Shell", hint: "Sessions in this repository — shells, agents, files and splits" },
  { id: "agents", label: "Agents", hint: "Agent sessions here and in other repositories, and launchers for new ones" },
  { id: "files", label: "Files", hint: "Browse this checkout and open files beside your sessions" },
  { id: "console", label: "Console", hint: "Run single commands with capped output and per-run policy verdicts" },
];

export const DOCK_SECTION_STORAGE_KEY = "gitpulse.dock.section";

export function normalizeDockSection(raw: unknown): DockSection {
  return raw === "agents" || raw === "files" || raw === "console" || raw === "shell" ? raw : "shell";
}

/**
 * The section chord: ⌥⇧⌘1–4 on macOS, Ctrl+Alt+Shift+1–4 elsewhere.
 *
 * Every lighter digit chord already has an owner: ⌘/Ctrl+digit pick
 * repository tabs, ⌥⌘digit (macOS) / Ctrl+Shift+digit pick views, ⌥digit
 * picks a view's section, and ⌥⌘digit / Ctrl+Alt+digit jump between split
 * panes inside the dock (`terminalSplitChord`). Adding Shift to the split
 * chord is the one free combination, and it keeps the dock's chords together.
 * Matched on `event.code` because Shift and Alt rewrite `event.key`.
 */
export function dockSectionForChord(
  event: Pick<KeyboardEvent, "code" | "altKey" | "ctrlKey" | "metaKey" | "shiftKey"> & { key?: string },
  os: string,
): DockSection | null {
  if (!event.altKey || !event.shiftKey) return null;
  const mac = os === "macos";
  if (mac ? !event.metaKey || event.ctrlKey : !event.ctrlKey || event.metaKey) return null;
  const digit = /^Digit([1-4])$/.exec(event.code ?? "")?.[1];
  if (!digit) return null;
  return DOCK_SECTIONS[Number(digit) - 1]?.id ?? null;
}

/** The chord a section answers to, for tooltips and the shortcut guide. */
export function dockSectionChordLabel(section: DockSection, os: HostOS): string {
  const index = DOCK_SECTIONS.findIndex((entry) => entry.id === section) + 1;
  return platformChord(`⌥⇧⌘${index}`, `Ctrl+Alt+Shift+${index}`, os);
}

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export interface DockSectionStore extends Readable<DockSection> {
  set(section: DockSection): void;
}

/**
 * The last section the reader chose, remembered across launches. A new
 * panel starts there; Shell is the default and the fallback.
 */
export function createDockSectionStore(
  storage: StorageLike | null | undefined = globalThis.localStorage,
): DockSectionStore {
  let initial: DockSection = "shell";
  try {
    initial = normalizeDockSection(storage?.getItem(DOCK_SECTION_STORAGE_KEY));
  } catch {
    /* storage unavailable */
  }
  const store = writable<DockSection>(initial);
  return {
    subscribe: store.subscribe,
    set(section) {
      const next = normalizeDockSection(section);
      store.set(next);
      try {
        storage?.setItem(DOCK_SECTION_STORAGE_KEY, next);
      } catch {
        /* storage unavailable or quota exceeded */
      }
    },
  };
}

export const dockSection = createDockSectionStore();

/**
 * The header's one status slot: the most pressing of the checkout's
 * warnings, with the rest summarised in its tooltip. Conflicts outrank
 * collisions, which outrank a collision check that could not run.
 */
export type DockStatus =
  | { readonly kind: "conflict"; readonly tone: "danger"; readonly label: string }
  | { readonly kind: "collision"; readonly tone: "warn"; readonly label: string }
  | { readonly kind: "unverified"; readonly tone: "neutral"; readonly label: string };

export function dockStatus(input: {
  readonly conflictedCount: number;
  readonly collision:
    | { readonly ok: boolean; readonly overlapping_files: number; readonly shared_worktree_files: number }
    | null
    | undefined;
}): { primary: DockStatus | null; others: readonly DockStatus[] } {
  const all: DockStatus[] = [];
  const n = input.conflictedCount;
  if (n > 0) all.push({ kind: "conflict", tone: "danger", label: `${n} ${n === 1 ? "conflict" : "conflicts"}` });
  const c = input.collision;
  if (c && !c.ok) {
    all.push({ kind: "unverified", tone: "neutral", label: "Collisions unverified" });
  } else if (c && c.overlapping_files > 0) {
    const k = c.overlapping_files;
    all.push({ kind: "collision", tone: "warn", label: `${k} cross-worktree ${k === 1 ? "collision" : "collisions"}` });
  } else if (c && c.shared_worktree_files > 0) {
    const k = c.shared_worktree_files;
    all.push({ kind: "collision", tone: "warn", label: `${k} shared dirty ${k === 1 ? "file" : "files"}` });
  }
  const rank = { conflict: 0, collision: 1, unverified: 2 } as const;
  all.sort((a, b) => rank[a.kind] - rank[b.kind]);
  return { primary: all[0] ?? null, others: all.slice(1) };
}
