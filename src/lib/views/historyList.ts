import { writable } from "svelte/store";

/**
 * Which list History › Graph draws above the commit details.
 *
 * Three lists of commits, one details pane:
 *
 * - `commits` is the graph walk.
 * - `moves` is the reflog: every movement of HEAD, including commits a reset
 *   or amend left reachable only from there.
 * - `suspects` reads the history backwards: from a misbehaving symbol to the
 *   commits since a known-good ref that touched what it reaches, ranked by the
 *   call graph rather than by date.
 *
 * Reflog and Suspects were History sections of their own. Both folded into
 * Graph because each is a list of commits, and as separate panes a click
 * either selected a commit on a Graph nobody was looking at (reflog) or left
 * the list altogether for Diff (suspects). Here every list selects through one
 * path (`repoStore.selectHistoryCommit`), the details pane stays underneath,
 * and switching lists keeps the selection.
 */
export type HistoryList = "commits" | "moves" | "suspects";

/** The toggle above the list, in order. Read by History and the palette. */
export const HISTORY_LISTS: readonly { value: HistoryList; label: string; title: string }[] = [
  { value: "commits", label: "Commits", title: "The commit graph: branches, merges and the selected commit" },
  {
    value: "moves",
    label: "HEAD moves",
    title: "Reflog: every movement of HEAD, with recovery points after a reset, checkout or amend",
  },
  {
    value: "suspects",
    label: "Suspects",
    title:
      "Regression suspects: which commit since a known-good ref could have caused a symptom, ranked by what the call graph reaches",
  },
];

/** The section id History's reflog had before it folded into Graph. */
export const RETIRED_REFLOG_SECTION = "reflog";

/** The section id History's regression suspects had before they folded into Graph. */
export const RETIRED_SUSPECTS_SECTION = "suspects";

/**
 * Narrows a stored value. Strict: anything but a known list id (absent, a
 * hand-edited blob, an older build's field) is the commit graph, which is
 * where History opens by default.
 */
export function sanitizeHistoryList(raw: unknown): HistoryList {
  return raw === "moves" || raw === "suspects" ? raw : "commits";
}

/**
 * The list a restored tab opens on.
 *
 * Besides the field itself, a session written before a fold names its list by
 * having been left on a retired section (`viewSections.history` = "reflog" or
 * "suspects") or on the older top-level Reflog view (`viewTab`). Each lands on
 * Graph through the registry's `RETIRED_SECTIONS` (and `RETIRED_VIEWS` for the
 * old view); this is what keeps it on the right list.
 */
export function restoredHistoryList(record: {
  historyList?: unknown;
  viewSections?: unknown;
  viewTab?: unknown;
}): HistoryList {
  // A retired section wins over the stored list. Only a build that still had
  // the section can have written it, and that build recorded the list Graph was
  // last left on whatever section was showing — so beside `history: "suspects"`
  // a stored "moves" is a stale Graph setting, not what was on screen.
  const sections = record.viewSections;
  if (sections && typeof sections === "object" && !Array.isArray(sections)) {
    const history = (sections as Record<string, unknown>).history;
    if (history === RETIRED_SUSPECTS_SECTION) return "suspects";
    if (history === RETIRED_REFLOG_SECTION) return "moves";
  }
  if (record.viewTab === RETIRED_REFLOG_SECTION) return "moves";
  return sanitizeHistoryList(record.historyList);
}

/**
 * What the selected commit is called in the list that selected it, when that
 * is more than its hash: a reflog selector (`HEAD@{3}`). The details pane's
 * actions name it in their prompts. Keyed by commit, so a selection made
 * anywhere else reads as unlabelled rather than inheriting a stale name.
 */
export const historySelectionLabel = writable<{ commitId: string; label: string } | null>(null);

export function labelFor(
  held: { commitId: string; label: string } | null,
  commitId: string | null | undefined,
): string | undefined {
  return held && commitId && held.commitId === commitId ? held.label : undefined;
}
