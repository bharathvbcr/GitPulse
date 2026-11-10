import { get, writable } from "svelte/store";

/**
 * How Code → Explorer is reading the repository right now.
 *
 * Search and Blame used to be sections of Code beside Explorer. Each was a
 * second reading of the same subject — Search a way to find the file, Blame
 * another way to read it — and each sent the user back to Explorer for the
 * thing it had just named. They are toggles inside Explorer now:
 *
 *  - `rail` is the side column: the file tree, or content search. A search
 *    hit opens in the editor beside its results instead of replacing them.
 *  - `lens` is how the open file is shown: the editor, or its blame. It
 *    follows the active tab, so blame stays on while you move between files.
 *
 * It lives outside `FileViewer` so the command palette can open Explorer
 * already showing Search or Blame (`code:search`, `code:blame`). It is not persisted:
 * which lens was up is a navigation choice, not session state to restore.
 */
export type ExplorerRail = "files" | "search";
export type EditorLens = "edit" | "blame";

export interface ExplorerLensState {
  rail: ExplorerRail;
  lens: EditorLens;
  /**
   * Set by an explicit request to search and cleared by the search box that
   * takes focus for it. A flag rather than a counter, because the request is
   * usually made before Explorer has mounted (the palette, from another view)
   * and must still land; and one-shot, so Explorer remounting with the rail
   * already on Search never pulls focus out of the editor.
   */
  searchFocusPending: boolean;
}

const INITIAL: ExplorerLensState = { rail: "files", lens: "edit", searchFocusPending: false };

const state = writable<ExplorerLensState>(INITIAL);

export const explorerLens = { subscribe: state.subscribe };

/** Shows `rail` in the side column. Asking for Search also asks for focus. */
export function showExplorerRail(rail: ExplorerRail): void {
  state.update((s) => ({
    ...s,
    rail,
    searchFocusPending: rail === "search" ? true : s.searchFocusPending,
  }));
}

/** Search when the tree is up, the tree when Search is. */
export function toggleExplorerSearch(): void {
  showExplorerRail(get(state).rail === "search" ? "files" : "search");
}

/** True once per request to search: the caller should focus its box. */
export function consumeSearchFocus(): boolean {
  if (!get(state).searchFocusPending) return false;
  state.update((s) => ({ ...s, searchFocusPending: false }));
  return true;
}

export function setEditorLens(lens: EditorLens): void {
  state.update((s) => (s.lens === lens ? s : { ...s, lens }));
}

export function toggleBlameLens(): void {
  state.update((s) => ({ ...s, lens: s.lens === "blame" ? "edit" : "blame" }));
}

/** Test seam: back to the tree and the editor. */
export function resetExplorerLens(): void {
  state.set(INITIAL);
}
