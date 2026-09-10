/**
 * The Tasks page's open-editor tab model.
 *
 * Pure on purpose: which task stays focused after a close, whether a draft
 * survives a switch, and when the strip is full are rules worth testing
 * without mounting TaskEditor or talking to the workbench store.
 */

import { writable } from "svelte/store";
import type { TaskStatus } from "./client";
import { displayTitle } from "./taskDelete";

/**
 * Ceiling on editors in the strip. Past this the board refuses to open
 * another rather than silently dropping the oldest — a missing task the
 * reader still sees on the board is clearer than one that vanished from
 * the strip.
 */
export const MAX_TASK_TABS = 12;

/** Shared tabpanel id: one editor is mounted, every tab points at it. */
export const TASK_EDITOR_PANE_ID = "gitpulse-task-editor-pane";

export interface TaskTab {
  id: string;
  title: string;
  status: TaskStatus | null;
  draft: boolean;
}

export interface TaskTabState {
  tabs: TaskTab[];
  activeId: string | null;
}

export interface TaskChrome {
  /** Editors currently in the Tasks strip, including drafts. */
  openTabs: number;
  /** In-progress cards on the loaded board (kanban, not editors). */
  inProgress: number;
}

export function emptyTaskTabs(): TaskTabState {
  return { tabs: [], activeId: null };
}

export function emptyTaskChrome(): TaskChrome {
  return { openTabs: 0, inProgress: 0 };
}

/** Chip on the repository tab bar reads this; only the global board writes it. */
export const taskChrome = writable<TaskChrome>(emptyTaskChrome());

function asCount(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.min(Math.floor(value), 10_000) : 0;
}

export function publishTaskChrome(next: TaskChrome): void {
  taskChrome.set({ openTabs: asCount(next.openTabs), inProgress: asCount(next.inProgress) });
}

export function taskTabLabel(tab: TaskTab): string {
  const title = tab.draft ? (tab.title.trim() || "New task") : tab.title;
  return displayTitle(title, 28);
}

export function canOpenTaskTab(state: TaskTabState, id: string): boolean {
  if (!id) return false;
  return state.tabs.some((tab) => tab.id === id) || state.tabs.length < MAX_TASK_TABS;
}

/**
 * Opens a tab and focuses it, or returns the state unchanged at the ceiling.
 *
 * Reopening an existing id always focuses it, even when the strip is full —
 * the ceiling is "how many distinct editors", not "how many activations".
 */
export function openTaskTab(state: TaskTabState, tab: TaskTab): TaskTabState {
  if (!tab.id) return state;
  const existing = state.tabs.findIndex((item) => item.id === tab.id);
  if (existing >= 0) {
    const tabs = state.tabs.map((item, index) => (index === existing ? { ...item, ...tab, id: item.id } : item));
    return { tabs, activeId: tab.id };
  }
  if (state.tabs.length >= MAX_TASK_TABS) return state;
  return { tabs: [...state.tabs, tab], activeId: tab.id };
}

/**
 * Closes a tab and picks the next focus.
 *
 * Neighbour rule matches the terminal: focus moves to the tab on the right,
 * or to the left when the closed tab was last. Closing an inactive tab
 * never moves focus. Closing the last tab empties the strip.
 */
export function closeTaskTab(state: TaskTabState, id: string): TaskTabState {
  const index = state.tabs.findIndex((tab) => tab.id === id);
  if (index === -1) return state;
  const tabs = state.tabs.filter((tab) => tab.id !== id);
  if (state.activeId !== id) return { tabs, activeId: state.activeId };
  const next = tabs[index] ?? tabs[index - 1] ?? null;
  return { tabs, activeId: next?.id ?? null };
}

export function activateTaskTab(state: TaskTabState, id: string): TaskTabState {
  if (!id || !state.tabs.some((tab) => tab.id === id)) return state;
  if (state.activeId === id) return state;
  return { ...state, activeId: id };
}

export function updateTaskTab(
  state: TaskTabState,
  id: string,
  patch: Partial<Pick<TaskTab, "title" | "status" | "draft">>,
): TaskTabState {
  if (!state.tabs.some((tab) => tab.id === id)) return state;
  return {
    ...state,
    tabs: state.tabs.map((tab) => (tab.id === id ? { ...tab, ...patch } : tab)),
  };
}

/**
 * A draft that has been saved takes the saved task's id. If that id is
 * already open, the draft tab is dropped and the existing one is focused —
 * two tabs for one task would make Close ambiguous.
 */
export function retargetTaskTab(state: TaskTabState, fromId: string, to: TaskTab): TaskTabState {
  if (!fromId || !to.id) return state;
  const from = state.tabs.findIndex((tab) => tab.id === fromId);
  if (from < 0) return state;
  if (fromId === to.id) return updateTaskTab(state, fromId, to);
  const collision = state.tabs.findIndex((tab) => tab.id === to.id);
  if (collision >= 0) {
    const tabs = state.tabs.filter((tab) => tab.id !== fromId);
    return { tabs, activeId: to.id };
  }
  const tabs = state.tabs.map((tab, index) => (index === from ? { ...tab, ...to } : tab));
  return { tabs, activeId: state.activeId === fromId ? to.id : state.activeId };
}

/** Saved task ids currently in the strip — drafts do not mark a board card. */
export function openSavedTaskIds(state: TaskTabState): ReadonlySet<string> {
  return new Set(state.tabs.filter((tab) => !tab.draft).map((tab) => tab.id));
}
