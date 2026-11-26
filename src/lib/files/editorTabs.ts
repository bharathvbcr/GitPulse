/**
 * VS Code-style editor tabs: a single preview tab is reused on single-click
 * browse; double-click (or an explicit pin) promotes it to a permanent tab.
 */

import { formatPathParts } from "./formatPath";

export interface EditorTab {
  path: string;
  name: string;
  preview: boolean;
}

export interface EditorTabState {
  tabs: EditorTab[];
  active: string | null;
}

function makeTab(path: string, preview: boolean): EditorTab {
  return { path, name: formatPathParts(path).name, preview };
}

export function emptyEditorTabs(): EditorTabState {
  return { tabs: [], active: null };
}

/**
 * Single-click: reuse or replace the preview tab. Pinned matches just activate.
 * `isDirty` says whether a path owns unsaved work (the drafts live in the
 * open-file store, not in the tabs).
 */
export function openPreview(
  state: EditorTabState,
  path: string,
  isDirty: (path: string) => boolean = () => false,
): EditorTabState {
  if (!path) return state;
  // A preview becomes permanent as soon as it owns unsaved work. This keeps a
  // subsequent single-click browse from becoming a destructive operation.
  const currentTabs = state.tabs.map((tab) =>
    tab.preview && isDirty(tab.path) ? { ...tab, preview: false } : tab,
  );
  const existing = currentTabs.findIndex((tab) => tab.path === path);
  if (existing >= 0) {
    return { ...state, tabs: currentTabs, active: path };
  }
  const previewIdx = currentTabs.findIndex((tab) => tab.preview);
  const next = makeTab(path, true);
  if (previewIdx >= 0) {
    const tabs = currentTabs.slice();
    tabs[previewIdx] = next;
    return { ...state, tabs, active: path };
  }
  return { ...state, tabs: [...currentTabs, next], active: path };
}

/** Double-click / pin: keep a dedicated tab that browse will not replace. */
export function openPinned(state: EditorTabState, path: string): EditorTabState {
  if (!path) return state;
  const existing = state.tabs.findIndex((tab) => tab.path === path);
  if (existing >= 0) {
    return { ...pinEditorTab(state, path), active: path };
  }
  return { ...state, tabs: [...state.tabs, makeTab(path, false)], active: path };
}

/** Pin an existing tab without changing the user's current selection. */
export function pinEditorTab(state: EditorTabState, path: string): EditorTabState {
  const existing = state.tabs.findIndex((tab) => tab.path === path);
  if (existing < 0 || !state.tabs[existing]?.preview) return state;
  const tabs = state.tabs.map((tab, index) =>
    index === existing ? { ...tab, preview: false } : tab,
  );
  return { ...state, tabs };
}

/** Activate an already-open tab without changing its preview/pinned state. */
export function activateEditorTab(state: EditorTabState, path: string): EditorTabState {
  if (!path || !state.tabs.some((tab) => tab.path === path)) return state;
  if (state.active === path) return state;
  return { ...state, active: path };
}

export function closeEditorTab(state: EditorTabState, path: string): EditorTabState {
  const idx = state.tabs.findIndex((tab) => tab.path === path);
  if (idx < 0) return state;
  const tabs = state.tabs.filter((tab) => tab.path !== path);
  if (state.active !== path) return { tabs, active: state.active };
  const neighbor = tabs[Math.min(idx, tabs.length - 1)];
  return { tabs, active: neighbor?.path ?? null };
}

/**
 * Close exactly the tabs captured by an earlier request. Tabs opened while an
 * asynchronous save or confirmation was pending are deliberately preserved.
 */
export function closeEditorTabs(
  state: EditorTabState,
  paths: readonly string[],
): EditorTabState {
  const closing = new Set(paths);
  let next = state;
  for (const tab of state.tabs) {
    if (closing.has(tab.path)) next = closeEditorTab(next, tab.path);
  }
  return next;
}

export function closeOtherEditorTabs(state: EditorTabState, keepPath: string): EditorTabState {
  const keep = state.tabs.find((tab) => tab.path === keepPath);
  if (!keep) return state;
  return { tabs: [{ ...keep, preview: false }], active: keepPath };
}

export function closeAllEditorTabs(): EditorTabState {
  return emptyEditorTabs();
}

/** Dirty paths among the exact tabs an operation intends to remove, in tab order. */
export function dirtyEditorTabPaths(
  state: EditorTabState,
  candidatePaths: readonly string[],
  isDirty: (path: string) => boolean,
): string[] {
  const candidates = new Set(candidatePaths);
  return state.tabs
    .filter((tab) => candidates.has(tab.path) && isDirty(tab.path))
    .map((tab) => tab.path);
}
