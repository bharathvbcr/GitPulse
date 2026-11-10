/**
 * Which list History › Graph draws above the commit details.
 *
 * `commits` is the graph walk. `moves` is the reflog: every movement of HEAD,
 * including commits a reset or amend left reachable only from there. The
 * reflog was its own History section; it folded into Graph because both are
 * lists of commits feeding the same details pane, and a reflog click used to
 * select a commit on a pane nobody was looking at.
 */
export type HistoryList = "commits" | "moves";

/** The section id History's reflog had before it folded into Graph. */
export const RETIRED_REFLOG_SECTION = "reflog";

/**
 * Narrows a stored value. Strict: anything but the literal `"moves"` (absent,
 * a hand-edited blob, an older build's field) is the commit graph, which is
 * where History opens by default.
 */
export function sanitizeHistoryList(raw: unknown): HistoryList {
  return raw === "moves" ? "moves" : "commits";
}

/**
 * The list a restored tab opens on.
 *
 * Besides the field itself, a session written before the fold says "moves" by
 * having been left on the retired Reflog section (`viewSections.history`) or
 * on the older top-level Reflog view (`viewTab`). Both land on Graph through
 * the registry's `RETIRED_SECTIONS` (and `RETIRED_VIEWS` for the old view);
 * this is what keeps them on the reflog.
 */
export function restoredHistoryList(record: {
  historyList?: unknown;
  viewSections?: unknown;
  viewTab?: unknown;
}): HistoryList {
  if (sanitizeHistoryList(record.historyList) === "moves") return "moves";
  if (record.viewTab === RETIRED_REFLOG_SECTION) return "moves";
  const sections = record.viewSections;
  if (sections && typeof sections === "object" && !Array.isArray(sections)) {
    if ((sections as Record<string, unknown>).history === RETIRED_REFLOG_SECTION) return "moves";
  }
  return "commits";
}
