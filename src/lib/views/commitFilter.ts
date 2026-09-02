import type { ViewTab } from "../repos/persist";

/**
 * Views where the commit-search filter bar actually filters what is on
 * screen. Everywhere else it looks like a page search and is not one.
 *
 * Declared as a set so a new view cannot inherit the bar by accident: it
 * has to opt in here.
 */
const COMMIT_FILTER_VIEWS: ReadonlySet<ViewTab> = new Set([
  "history",
  "diff",
  "blame",
  "stack",
  "reflog",
]);

/** Window event FilterBar listens for to focus the commit-search input. */
export const FOCUS_COMMIT_SEARCH_EVENT = "gitpulse:focus-filter";

export function showsCommitFilter(tab: ViewTab): boolean {
  return COMMIT_FILTER_VIEWS.has(tab);
}

/**
 * Files owns ⌘F for in-file search on the code viewer. Every other view
 * either shows the commit-search bar or should switch to Graph so the
 * chord is not a silent no-op (Work used to swallow it).
 */
export function ownsCommitSearchChord(tab: ViewTab): boolean {
  return tab !== "files";
}

/** Native "Search Commits" always lands on a view the bar actually filters. */
export function tabForCommitSearch(tab: ViewTab): ViewTab {
  return showsCommitFilter(tab) ? tab : "history";
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
