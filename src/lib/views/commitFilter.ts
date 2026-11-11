import type { ViewTab } from "../repos/persist";

/**
 * Who owns ⌘F, and where "Search Commits" lands.
 *
 * The answer used to depend on the pane: History's graph sent the chord to
 * its commit filter, while Code and History's diff caught it in their own
 * toolbars and opened a find bar there. There is one search field now, in
 * the view bar, and it adapts to the page (`search/pageSearch.svelte.ts`):
 * it filters History's commits, finds in a diff or a file, and searches the
 * repository through the command palette where the page has nothing of its
 * own. So on the repository surface ⌘F always means "the field", and the
 * page decides what the field does. A pane that handled the key itself (the
 * terminal's own find) marks it handled, and the field leaves it alone.
 */

/** The view whose list the commit filter narrows. */
export const COMMIT_FILTER_VIEW: ViewTab = "history";

/** The section of that view the filter belongs to. */
export const COMMIT_FILTER_SECTION = "graph";

/** Window event the view bar's search field listens for to take focus. */
export const FOCUS_PAGE_SEARCH_EVENT = "gitpulse:focus-page-search";

export function showsCommitFilter(tab: ViewTab): boolean {
  return tab === COMMIT_FILTER_VIEW;
}

/** Native "Search Commits" always lands on the view the filter narrows. */
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

/**
 * ⌘F belongs to the view bar's field whenever the field is on screen: a
 * repository is open and the repository surface (not Fleet, Agents or Tasks)
 * is showing. A key another handler already took is not the field's.
 */
export function fieldOwnsFindChord(state: {
  hasRepository: boolean;
  onRepositorySurface: boolean;
  defaultPrevented: boolean;
}): boolean {
  return state.hasRepository && state.onRepositorySurface && !state.defaultPrevented;
}
