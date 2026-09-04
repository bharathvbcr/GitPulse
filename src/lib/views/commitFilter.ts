import type { ViewTab } from "../repos/persist";

/**
 * Where the commit filter lives, and who owns ⌘F.
 *
 * The bar used to be a full-width strip App stacked above the sidebar for
 * five different views. Those five are one view now — History, whose Graph,
 * Diff and Reflog sections are all drawn from the same filtered walk — so the
 * bar moved into that view's own section bar, and this module shrank to the
 * one question the rest of the app still asks: who handles the chord.
 */

/** The only view that renders the commit filter. */
export const COMMIT_FILTER_VIEW: ViewTab = "history";

/** Window event FilterBar listens for to focus the commit-search input. */
export const FOCUS_COMMIT_SEARCH_EVENT = "gitpulse:focus-filter";

export function showsCommitFilter(tab: ViewTab): boolean {
  return tab === COMMIT_FILTER_VIEW;
}

/**
 * Code owns ⌘F for in-file search on the code viewer. Every other view either
 * shows the commit-search bar or switches to History so the chord is not a
 * silent no-op (Work used to swallow it).
 *
 * Deliberately per view, not per section: Blame is a section of Code, and its
 * lines are the same file's lines. Handing the chord to commit search there
 * would mean ⌘F did one thing on a file and a different thing on the same
 * file one click later.
 */
export function ownsCommitSearchChord(tab: ViewTab): boolean {
  return tab !== "code";
}

/** Native "Search Commits" always lands on the view the bar actually filters. */
export function tabForCommitSearch(tab: ViewTab): ViewTab {
  return showsCommitFilter(tab) ? tab : COMMIT_FILTER_VIEW;
}

export function isCommitSearchChord(e: {
  metaKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
  key?: string;
}): boolean {
  if (e.altKey || e.shiftKey) return false;
  if (!(e.metaKey || e.ctrlKey)) return false;
  return (e.key ?? "").toLowerCase() === "f";
}
