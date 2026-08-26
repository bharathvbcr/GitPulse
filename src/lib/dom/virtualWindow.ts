/**
 * Pure windowing math for fixed-row-height virtual lists.
 *
 * `start` is the first row to render and `end` one past the last, clamped to
 * `[0, totalRows]`. The band is
 * `[max(0, firstVisible - overscan), min(totalRows, lastVisibleExclusive + overscan)]`
 * so scrolling never exposes an unpainted edge; it is computed from the
 * unclamped first-visible row so a top-clamped start cannot add the top
 * overscan twice. A non-finite scrollTop fails closed and paints nothing.
 */
export interface VirtualWindow {
  start: number;
  end: number;
}

/**
 * Clamps a scroll position to the largest offset that still shows content:
 * `min(scrollTop, max(0, total * rowHeight - viewportHeight))`.
 *
 * VirtualList derives its window from this instead of the raw bindable, so a
 * list that shrinks under a deep anchor (whitespace collapse, file switch)
 * paints its tail on the very first frame instead of the empty
 * `{total, total}` window `computeWindow` returns for offsets past the end.
 * The browser's own asynchronous clamp then converges the bindable back via
 * the scroll event. Every degenerate input fails closed to 0 — a non-finite
 * anchor has no position worth preserving, and unmeasurable geometry has no
 * scrollable content — rather than leaking NaN out of `Math.min`.
 */
export function clampScrollTop(
  scrollTop: number,
  total: number,
  rowHeight: number,
  viewportHeight: number
): number {
  if (!Number.isFinite(scrollTop)) return 0;
  const safeScrollTop = Math.max(0, scrollTop);
  if (!Number.isFinite(total) || total <= 0 || !Number.isFinite(rowHeight) || rowHeight <= 0) {
    return 0;
  }
  // A non-finite or negative viewport cannot bound scrolling either. Zero is
  // a legitimate pre-measurement state and just caps at the spacer height.
  if (!Number.isFinite(viewportHeight) || viewportHeight < 0) return 0;
  const maxScroll = Math.max(0, total * rowHeight - viewportHeight);
  return Math.min(safeScrollTop, maxScroll);
}

export function computeWindow(
  scrollTop: number,
  viewportHeight: number,
  totalRows: number,
  rowHeight: number,
  overscan: number
): VirtualWindow {
  if (!Number.isFinite(totalRows) || totalRows <= 0 || !Number.isFinite(rowHeight) || rowHeight <= 0) {
    return { start: 0, end: 0 };
  }
  if (!Number.isFinite(scrollTop)) {
    // An unknown scroll anchor paints nothing instead of guessing the top.
    return { start: 0, end: 0 };
  }
  const safeScrollTop = Math.max(0, scrollTop);
  // A non-finite overscan must clamp to 0 like every other degenerate input:
  // Math.max(0, Math.floor(NaN)) is NaN and would poison start/end below.
  // Positive infinity overscan renders the whole list.
  const scan =
    overscan === Number.POSITIVE_INFINITY
      ? totalRows
      : Number.isFinite(overscan)
        ? Math.max(0, Math.floor(overscan))
        : 0;
  const firstVisible = Math.floor(safeScrollTop / rowHeight);
  // A scroll position past the final row (elastic overscroll, stale spacer
  // height) must not produce a start beyond the list.
  const start = Math.max(0, Math.min(firstVisible - scan, totalRows));
  const visibleCount =
    Number.isFinite(viewportHeight) && viewportHeight > 0 ? Math.ceil(viewportHeight / rowHeight) : 0;
  // Covers [firstVisible - scan, firstVisible + visible + scan): one overscan
  // band on each side of what is actually on screen.
  const end = Math.max(start, Math.min(totalRows, firstVisible + visibleCount + scan));
  return { start, end };
}

/**
 * Last-line guarantee over a computed window: if renderable content exists
 * (finite positive total, row height and viewport) but the band collapsed to
 * empty, paint one real row instead of a blank pane.
 *
 * `computeWindow` deliberately returns `{totalRows, totalRows}` for anchors
 * past the content, and even a clamped anchor can land there through pure
 * float round-trip: at the cap, `fl(fl(total * rowHeight) - viewportHeight)`
 * divided back by `rowHeight` can overshoot the last row by more than the
 * overscan (one ulp of a 1e300-row product dwarfs any sane overscan). The
 * component runs this after `computeWindow` so the tail frame always shows
 * the final row; anything already non-empty — or degenerate geometry, or
 * totals beyond array scale where "row N" has no identity — passes through
 * untouched.
 */
export function ensureNonEmptyWindow(
  win: VirtualWindow,
  totalRows: number,
  rowHeight: number,
  viewportHeight: number
): VirtualWindow {
  if (
    win.end > win.start ||
    !Number.isFinite(totalRows) ||
    totalRows <= 0 ||
    totalRows > Number.MAX_SAFE_INTEGER ||
    !Number.isFinite(rowHeight) ||
    rowHeight <= 0 ||
    !Number.isFinite(viewportHeight) ||
    viewportHeight <= 0
  ) {
    return win;
  }
  const start = Math.max(0, Math.min(win.start, totalRows - 1));
  const end = Math.min(totalRows, start + 1);
  // Beyond-safe-integer arithmetic can leave start+1 unrepresentable; then
  // no band is expressible and the input window is returned as-is.
  return end > start ? { start, end } : win;
}
