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
  const scan = Math.max(0, Math.floor(overscan));
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
