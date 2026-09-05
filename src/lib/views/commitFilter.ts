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
 * Views that read a file's lines own ⌘F for searching those lines; everything
 * else either shows the commit-search bar or switches to History so the chord
 * is not a silent no-op (Work used to swallow it).
 *
 * The rule is "does this pane show lines of code" — not "which view is it".
 * Code says no for both its sections, because Blame's lines are the Explorer's
 * lines and ⌘F must not mean one thing on a file and something else on the
 * same file one click later. History's Diff section shows lines too, which is
 * why the section matters here: Graph and Reflog are lists of commits and
 * ⌘F belongs to the filter that narrows them, while Diff is a file — or a
 * commit's worth of files — and ⌘F belongs to the find bar inside it.
 *
 * The section argument is optional so callers that only know the view keep
 * working; omitting it answers for the view's sections that are not diffs,
 * which is the safe direction (the commit filter always exists in History).
 */
export function ownsCommitSearchChord(tab: ViewTab, section?: string | null): boolean {
  if (tab === "code") return false;
  if (tab === "history" && section === "diff") return false;
  return true;
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
