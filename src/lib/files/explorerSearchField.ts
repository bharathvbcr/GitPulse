import type { ExplorerRail } from "./explorerLens";

/**
 * Explorer has one search field. By default it filters the tree by file name
 * as you type; one small toggle inside the field switches it to searching
 * inside the files. The mode is the rail (`explorerLens`), so these are the
 * pure decisions the field makes from it.
 *
 * The toggle replaced a Names | Contents segment. Two word buttons did not fit
 * beside the text in a narrow column (the main sidebar is 264px at its
 * narrowest), so the segment dropped to a second row and turned the pill into
 * a tall box. An icon toggle keeps the field one row at every width.
 */

/** The rail the toggle moves to from `mode`: names and contents swap. */
export function toggledSearchMode(mode: ExplorerRail): ExplorerRail {
  return mode === "search" ? "files" : "search";
}

/**
 * The toggle's accessible name. Constant: `aria-pressed` carries the state.
 * "Search inside files", not "Include file contents": pressed, it replaces the
 * name filter with content search rather than adding contents to it.
 */
export const CONTENTS_TOGGLE_LABEL = "Search inside files";

/** The toggle's tooltip, which says what clicking it will do. */
export function contentsToggleTitle(mode: ExplorerRail): string {
  return mode === "search"
    ? "Searching file contents. Click to filter by file name"
    : "Search inside files instead of file names";
}

/** The field's placeholder and accessible name, by mode. */
export function searchFieldLabel(mode: ExplorerRail): string {
  return mode === "search" ? "Search file contents" : "Filter files";
}

/** What Escape does in the field. */
export type FieldEscape = "clear" | "leave-contents" | "none";

/**
 * Escape clears whatever is typed; with nothing typed, Contents mode hands
 * the column back to the tree, and Names mode has nothing left to undo.
 */
export function fieldEscape(mode: ExplorerRail, value: string): FieldEscape {
  if (value !== "") return "clear";
  return mode === "search" ? "leave-contents" : "none";
}
