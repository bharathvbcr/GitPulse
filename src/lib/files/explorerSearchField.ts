import type { ExplorerRail } from "./explorerLens";

/**
 * Explorer has one search field with two modes: Names narrows the tree as you
 * type, Contents searches inside the files. The mode is the rail
 * (`explorerLens`), so these are the pure decisions the field makes from it.
 */

/** The two segments, in the order they read. */
export const EXPLORER_SEARCH_MODES: readonly { value: ExplorerRail; label: string; title: string }[] = [
  { value: "files", label: "Names", title: "Filter the file tree by name" },
  { value: "search", label: "Contents", title: "Search file contents across the repository" },
];

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
