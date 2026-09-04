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
  /** Unsaved editor content, owned by the same per-repository state as tabs. */
  drafts: Record<string, EditorDraft>;
}

export interface EditorDraft {
  content: string;
  /** File content against which this draft first became dirty. */
  sourceContent: string;
}

function makeTab(path: string, preview: boolean): EditorTab {
  return { path, name: formatPathParts(path).name, preview };
}

export function emptyEditorTabs(): EditorTabState {
  return { tabs: [], active: null, drafts: {} };
}

/** Single-click: reuse or replace the preview tab. Pinned matches just activate. */
export function openPreview(state: EditorTabState, path: string): EditorTabState {
  if (!path) return state;
  // A preview becomes permanent as soon as it owns unsaved work. This keeps a
  // subsequent single-click browse from becoming a destructive operation.
  const currentTabs = state.tabs.map((tab) =>
    tab.preview && isEditorTabDirty(state, tab.path) ? { ...tab, preview: false } : tab,
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
  const drafts = { ...state.drafts };
  delete drafts[path];
  if (state.active !== path) return { tabs, active: state.active, drafts };
  const neighbor = tabs[Math.min(idx, tabs.length - 1)];
  return { tabs, active: neighbor?.path ?? null, drafts };
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
  const draft = state.drafts[keepPath];
  return {
    tabs: [{ ...keep, preview: false }],
    active: keepPath,
    drafts: draft ? { [keepPath]: draft } : {},
  };
}

export function closeAllEditorTabs(): EditorTabState {
  return emptyEditorTabs();
}

export function editorDraft(state: EditorTabState, path: string): EditorDraft | undefined {
  return state.drafts[path];
}

export function isEditorTabDirty(state: EditorTabState, path: string): boolean {
  return editorDraft(state, path) !== undefined;
}

export function hasDirtyEditorTabs(state: EditorTabState): boolean {
  return Object.keys(state.drafts).length > 0;
}

/**
 * Store the latest input synchronously. Dirty previews are pinned so rapid
 * browse events cannot replace their tab before the next render.
 */
export function updateEditorDraft(
  state: EditorTabState,
  path: string,
  content: string,
  sourceContent: string,
): EditorTabState {
  if (!state.tabs.some((tab) => tab.path === path)) return state;
  const drafts = { ...state.drafts };
  if (content === sourceContent) {
    delete drafts[path];
  } else {
    drafts[path] = { content, sourceContent };
  }
  const tabs = content === sourceContent
    ? state.tabs
    : state.tabs.map((tab) => tab.path === path ? { ...tab, preview: false } : tab);
  return { tabs, active: state.active, drafts };
}

function clearEditorDraft(state: EditorTabState, path: string): EditorTabState {
  if (!state.drafts[path]) return state;
  const drafts = { ...state.drafts };
  delete drafts[path];
  return { ...state, drafts };
}

/** Successful writes are the only save-path callers allowed to clear dirty state. */
export function completeEditorSave(
  state: EditorTabState,
  path: string,
  savedContent: string,
): EditorTabState {
  const draft = state.drafts[path];
  // Input can race a slow write. Completing an older snapshot must not mark
  // newer keystrokes clean.
  if (draft && draft.content !== savedContent) return state;
  return clearEditorDraft(state, path);
}

/** Called only after the user explicitly confirms discarding this draft. */
export function discardEditorDraft(state: EditorTabState, path: string): EditorTabState {
  return clearEditorDraft(state, path);
}

/** Dirty paths among the exact tabs an operation intends to remove, in tab order. */
export function dirtyEditorTabPaths(
  state: EditorTabState,
  candidatePaths: readonly string[],
): string[] {
  const candidates = new Set(candidatePaths);
  return state.tabs
    .filter((tab) => candidates.has(tab.path) && isEditorTabDirty(state, tab.path))
    .map((tab) => tab.path);
}
