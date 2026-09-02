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

/** Single-click: reuse or replace the preview tab. Pinned matches just activate. */
export function openPreview(state: EditorTabState, path: string): EditorTabState {
  if (!path) return state;
  const existing = state.tabs.findIndex((tab) => tab.path === path);
  if (existing >= 0) {
    return { tabs: state.tabs, active: path };
  }
  const previewIdx = state.tabs.findIndex((tab) => tab.preview);
  const next = makeTab(path, true);
  if (previewIdx >= 0) {
    const tabs = state.tabs.slice();
    tabs[previewIdx] = next;
    return { tabs, active: path };
  }
  return { tabs: [...state.tabs, next], active: path };
}

/** Double-click / pin: keep a dedicated tab that browse will not replace. */
export function openPinned(state: EditorTabState, path: string): EditorTabState {
  if (!path) return state;
  const existing = state.tabs.findIndex((tab) => tab.path === path);
  if (existing >= 0) {
    const tabs = state.tabs.map((tab, index) =>
      index === existing ? { ...tab, preview: false } : tab,
    );
    return { tabs, active: path };
  }
  return { tabs: [...state.tabs, makeTab(path, false)], active: path };
}

/** Activate an already-open tab without changing its preview/pinned state. */
export function activateEditorTab(state: EditorTabState, path: string): EditorTabState {
  if (!path || !state.tabs.some((tab) => tab.path === path)) return state;
  if (state.active === path) return state;
  return { tabs: state.tabs, active: path };
}

export function closeEditorTab(state: EditorTabState, path: string): EditorTabState {
  const idx = state.tabs.findIndex((tab) => tab.path === path);
  if (idx < 0) return state;
  const tabs = state.tabs.filter((tab) => tab.path !== path);
  if (state.active !== path) return { tabs, active: state.active };
  const neighbor = tabs[Math.min(idx, tabs.length - 1)];
  return { tabs, active: neighbor?.path ?? null };
}

export function closeOtherEditorTabs(state: EditorTabState, keepPath: string): EditorTabState {
  const keep = state.tabs.find((tab) => tab.path === keepPath);
  if (!keep) return state;
  return { tabs: [{ ...keep, preview: false }], active: keepPath };
}

export function closeAllEditorTabs(): EditorTabState {
  return emptyEditorTabs();
}
