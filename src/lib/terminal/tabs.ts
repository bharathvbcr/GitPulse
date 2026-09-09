/**
 * The terminal's tab model.
 *
 * Pure on purpose: every rule that decides which shell survives a close, what
 * a tab is called, and when a new one may be opened is a rule worth testing
 * without a PTY, a webview, or an xterm instance behind it.
 */

import { isImeComposition } from "../keyboard/imeGuard";

/** What a tab runs. `shell` is the user's own login shell; the rest are agent CLIs. */
export type LauncherKind = "shell" | "claude" | "manvi" | "codex";

export const LAUNCHERS: readonly { kind: LauncherKind; label: string }[] = [
  { kind: "shell", label: "Shell" },
  { kind: "claude", label: "Claude" },
  { kind: "manvi", label: "Manvi" },
  { kind: "codex", label: "Codex" },
];

/**
 * Ceiling on open tabs, equal to the backend's `MAX_PTY_SESSIONS`.
 *
 * Deliberately the same number rather than a smaller "safe" one: a UI that
 * stopped short would make a reachable backend refusal unreachable, and a UI
 * that ran past it would surface that refusal as an unexplained spawn error.
 * `tabs.contract.test.ts` reads the Rust constant and fails if the two drift.
 */
export const MAX_TERMINAL_TABS = 16;

export interface TerminalTab {
  id: string;
  launcher: LauncherKind;
  /**
   * What the running program last called itself (an OSC 0/2 title), or null
   * while it has said nothing. Null is not "no title" — it is "nothing has
   * been reported yet", and {@link tabLabel} is what turns either into text.
   */
  title: string | null;
  name?: string;
  /** Supplied as a literal agent CLI argument when this tab starts. */
  initialPrompt?: string;
}

export interface TabState {
  tabs: TerminalTab[];
  activeId: string | null;
}

export function launcherLabel(kind: LauncherKind): string {
  return LAUNCHERS.find((l) => l.kind === kind)?.label ?? kind;
}

/**
 * The text on a tab: what the program calls itself, else what it was started
 * as. A shell-reported title is trimmed and clipped — a program may emit an
 * arbitrarily long one, and a tab strip is not the place to discover that.
 */
export function tabLabel(tab: TerminalTab): string {
  const reported = tab.name?.trim() || tab.title?.trim() || "";
  if (!reported) return launcherLabel(tab.launcher);
  return reported.length > 28 ? `${reported.slice(0, 27)}…` : reported;
}

let sequence = 0;

/** Ids are per-webview and only ever compared to each other. */
function nextTabId(): string {
  sequence += 1;
  return `tab-${sequence}`;
}

export function createTab(launcher: LauncherKind, initialPrompt?: string): TerminalTab {
  return { id: nextTabId(), launcher, title: null, ...(initialPrompt === undefined ? {} : { initialPrompt }) };
}

export function initialState(launcher: LauncherKind = "shell"): TabState {
  const tab = createTab(launcher);
  return { tabs: [tab], activeId: tab.id };
}

export function canOpenTab(state: TabState): boolean {
  return state.tabs.length < MAX_TERMINAL_TABS;
}

/**
 * Opens a tab and focuses it, or returns the state unchanged at the ceiling.
 *
 * Silent refusal is the caller's cue to have disabled the control already;
 * this returning the same object is what makes "nothing happened" checkable.
 */
export function openTab(state: TabState, launcher: LauncherKind, initialPrompt?: string): TabState {
  if (!canOpenTab(state)) return state;
  const tab = createTab(launcher, initialPrompt);
  return { tabs: [...state.tabs, tab], activeId: tab.id };
}

/**
 * Closes a tab and picks the next focus.
 *
 * The neighbour rule is the one every tabbed terminal uses: focus moves to the
 * tab on the right, or to the left when the closed tab was last — so closing
 * repeatedly walks a direction instead of jumping to an edge. Closing an
 * inactive tab never moves focus.
 *
 * Closing the last tab empties the strip rather than auto-opening a
 * replacement: the caller owns whether "no tabs" means "closed the dock" or
 * "start a fresh shell", and inventing a session here would spawn a process
 * nobody asked for.
 */
export function closeTab(state: TabState, id: string): TabState {
  const index = state.tabs.findIndex((tab) => tab.id === id);
  if (index === -1) return state;
  const tabs = state.tabs.filter((tab) => tab.id !== id);
  if (state.activeId !== id) return { tabs, activeId: state.activeId };
  const next = tabs[index] ?? tabs[index - 1] ?? null;
  return { tabs, activeId: next?.id ?? null };
}

export function activateTab(state: TabState, id: string): TabState {
  if (!state.tabs.some((tab) => tab.id === id)) return state;
  return { ...state, activeId: id };
}

/** Records a title the running program reported for one tab. */
export function setTabTitle(state: TabState, id: string, title: string): TabState {
  if (!state.tabs.some((tab) => tab.id === id)) return state;
  return {
    ...state,
    tabs: state.tabs.map((tab) => (tab.id === id ? { ...tab, title: cleanTabName(title, 256) } : tab)),
  };
}

/**
 * Focus one step along the strip, wrapping at both ends.
 *
 * Wrapping matters more than it looks: without it the chord dies silently at
 * whichever end you are already on, which reads as a broken shortcut.
 */
export function cycleTab(state: TabState, step: 1 | -1): TabState {
  if (state.tabs.length < 2) return state;
  const index = state.tabs.findIndex((tab) => tab.id === state.activeId);
  if (index === -1) return state;
  const next = (index + step + state.tabs.length) % state.tabs.length;
  return { ...state, activeId: state.tabs[next].id };
}

/** What a terminal keyboard chord asks the strip to do. */
export type TabChord = "new" | "close" | "next" | "prev" | "ignore";

/**
 * The chord this event is, or null when it is ordinary input.
 *
 * Control rather than Command throughout, matching the dock's own ⌃` toggle:
 * on macOS ⌘` is the OS window cycler and ⌘W would close the app window out
 * from under a running shell. Pure so the bindings are checkable without a
 * webview — the handler that consumes this sits inside xterm's key path, where
 * a wrong answer silently eats a keystroke the shell needed.
 */
export function terminalTabChord(event: {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
  repeat?: boolean;
}): TabChord | null {
  if (isImeComposition(event)) return null;
  if (!event.ctrlKey || event.metaKey || event.altKey) return null;
  if (event.key === "Tab") return event.shiftKey ? "prev" : "next";
  if (!event.shiftKey) return null;
  if (event.repeat && ["t", "w"].includes(event.key.toLowerCase())) return "ignore";
  // Shift makes `key` the uppercase letter; compare on one case only.
  switch (event.key.toLowerCase()) {
    case "t":
      return "new";
    case "w":
      return "close";
    default:
      return null;
  }
}

/** Arrow navigation belongs to the tab strip, never the shell's input. */
export function terminalTabDestination(state: TabState, key: string): string | null {
  if (!state.tabs.length) return null;
  if (key === "Home") return state.tabs[0].id;
  if (key === "End") return state.tabs[state.tabs.length - 1].id;
  if (key === "ArrowRight") return cycleTab(state, 1).activeId;
  if (key === "ArrowLeft") return cycleTab(state, -1).activeId;
  return null;
}

/** Titles are display data, never terminal control sequences or unbounded state. */
export function cleanTabName(value: string, limit = 64): string {
  return Array.from(value.slice(0, limit * 2).replace(/[\x00-\x1f\x7f-\x9f]/g, "").trim()).slice(0, limit).join("");
}

export function renameTab(state: TabState, id: string, name: string): TabState {
  return { ...state, tabs: state.tabs.map((tab) => tab.id === id ? { ...tab, name: cleanTabName(name) } : tab) };
}

export function moveTab(state: TabState, id: string, step: -1 | 1): TabState {
  const from = state.tabs.findIndex((tab) => tab.id === id);
  const to = from + step;
  if (from < 0 || to < 0 || to >= state.tabs.length) return state;
  const tabs = [...state.tabs];
  [tabs[from], tabs[to]] = [tabs[to], tabs[from]];
  return { ...state, tabs };
}
