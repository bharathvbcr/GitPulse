import type { ReflogEntry } from "./types";

/**
 * Narrows loaded reflog entries by History's commit-filter text.
 *
 * Matches, case-insensitively, the selector (`HEAD@{3}`), the commit id (a
 * prefix such as a pasted short sha matches), the action and the message.
 * Whitespace-separated terms must all match, the way a search box reads. An
 * empty query keeps every entry and returns the same array, so a list keyed
 * on it does not re-render.
 */
export function filterReflogEntries(entries: ReflogEntry[], query: string): ReflogEntry[] {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (terms.length === 0) return entries;
  return entries.filter((entry) => {
    const haystack = `${entry.selector}\n${entry.commit_id}\n${entry.action}\n${entry.message}`.toLowerCase();
    return terms.every((term) => haystack.includes(term));
  });
}
