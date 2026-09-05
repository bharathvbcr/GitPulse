/**
 * Bounds for in-file search.
 *
 * The scan is synchronous and runs between frames over every line of the open
 * file — up to `MAX_RENDER_LINES` of them. Two things kept that from being
 * safe: the query was bound straight to the input, so the whole file was
 * re-scanned on every keystroke, and the match list was unbounded, so one
 * common letter in a large file allocated hundreds of thousands of objects
 * before the next paint.
 *
 * Both numbers live here rather than in the component so the count label and
 * the scan cannot disagree about what the cap is.
 */

/** Quiet window before a typed query is scanned. */
export const SEARCH_DEBOUNCE_MS = 120;

/**
 * How a capped match count is written.
 *
 * A capped scan must not present itself as a complete one: "5000 matches" and
 * "5000+ matches" are different claims, and only the second is true when the
 * scan stopped at its ceiling.
 */
export function matchCountLabel(found: number, cap: number, current: number): string {
  if (found === 0) return "0 matches";
  const capped = found >= cap;
  const total = capped ? `${cap}+` : String(found);
  return `${current + 1} of ${total}`;
}
