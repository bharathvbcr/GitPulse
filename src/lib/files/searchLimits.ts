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
