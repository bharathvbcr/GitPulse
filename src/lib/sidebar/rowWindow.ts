/**
 * Windowing math for a virtual list whose rows are NOT all the same height.
 *
 * `dom/virtualWindow.ts` stays the owner of the fixed-row-height case (the
 * file list, where every row genuinely is one line). The sidebar's single
 * shared scroller is different: a section header, a folder header, a tag row
 * and a two-line branch row are four different heights in the same column, so
 * `index * rowHeight` is no longer the top of row `index`.
 *
 * The whole approach is one prefix-sum array. `offsets[i]` is the top edge of
 * row `i` and `offsets[n]` is the total scroll height, so every question the
 * component asks — which rows are on screen, where to scroll to reveal row
 * `i`, how tall the spacer is — is an O(log n) binary search or an O(1) index
 * instead of a multiply. Building it is O(n), the same cost as the
 * `flattenRows` pass that produced the rows, so nothing gets slower.
 *
 * The invariant every function here depends on is that `offsets` is
 * NON-DECREASING. That is what makes binary search meaningful; `buildRowOffsets`
 * is the only thing that constructs one and it guarantees it by coercing any
 * degenerate height to 0 rather than letting a NaN poison the whole tail (one
 * `NaN` in a prefix sum makes every later comparison false, which silently
 * empties the list).
 */

/** Half-open band of rows to render: `[start, end)`. */
export interface RowWindow {
  start: number;
  end: number;
}

/**
 * Prefix sums of `heights`, length `heights.length + 1`.
 *
 * `result[i]` is the top edge of row `i`; `result[heights.length]` is the
 * total height. A non-finite or negative height contributes 0: unmeasurable
 * geometry means that row occupies no space, which keeps the array
 * non-decreasing and leaves every other row's position correct. The
 * alternative — propagating NaN — would make the list paint nothing at all.
 */
export function buildRowOffsets(heights: readonly number[]): number[] {
  const offsets = new Array<number>(heights.length + 1);
  offsets[0] = 0;
  let running = 0;
  for (let i = 0; i < heights.length; i++) {
    const h = heights[i];
    running += Number.isFinite(h) && h > 0 ? h : 0;
    offsets[i + 1] = running;
  }
  return offsets;
}

/** Total scroll height described by `offsets`; 0 for an empty/degenerate array. */
export function totalRowHeight(offsets: readonly number[]): number {
  if (offsets.length === 0) return 0;
  const total = offsets[offsets.length - 1];
  return Number.isFinite(total) && total > 0 ? total : 0;
}

/** Number of rows `offsets` describes (one fewer than its length). */
export function rowCount(offsets: readonly number[]): number {
  return Math.max(0, offsets.length - 1);
}

/**
 * Top edge of row `index`, or 0 when the index is outside the array.
 *
 * Out-of-range fails closed to the top rather than throwing or returning
 * NaN: a caller scrolling to a row that filtering just removed lands at the
 * top of the list, which is a defensible position, instead of at `NaN`
 * (which browsers ignore, stranding the reader wherever they were with no
 * feedback).
 */
export function rowTop(offsets: readonly number[], index: number): number {
  if (!Number.isInteger(index) || index < 0 || index >= offsets.length - 1) return 0;
  const top = offsets[index];
  return Number.isFinite(top) && top > 0 ? top : 0;
}

/** Bottom edge of row `index` (top of the next row), or 0 when out of range. */
export function rowBottom(offsets: readonly number[], index: number): number {
  if (!Number.isInteger(index) || index < 0 || index >= offsets.length - 1) return 0;
  const bottom = offsets[index + 1];
  return Number.isFinite(bottom) && bottom > 0 ? bottom : 0;
}

/**
 * Smallest `i` in `[0, offsets.length)` with `offsets[i] > value`, else
 * `offsets.length`. Plain binary search over the non-decreasing prefix array.
 */
function firstOffsetAbove(offsets: readonly number[], value: number): number {
  let lo = 0;
  let hi = offsets.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (offsets[mid] > value) hi = mid;
    else lo = mid + 1;
  }
  return lo;
}

/**
 * Smallest `i` in `[0, offsets.length)` with `offsets[i] >= value`, else
 * `offsets.length`.
 */
function firstOffsetAtLeast(offsets: readonly number[], value: number): number {
  let lo = 0;
  let hi = offsets.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (offsets[mid] >= value) hi = mid;
    else lo = mid + 1;
  }
  return lo;
}

/**
 * Clamps a scroll position to the largest offset that still shows content.
 *
 * Same contract and the same reason as `clampScrollTop` in
 * `dom/virtualWindow.ts`: when a filter or density change shrinks the list
 * under a deep anchor, the raw `scrollTop` bindable is briefly past the end
 * and would paint one frame of nothing before the browser's asynchronous
 * clamp round-trips through the scroll event. Every degenerate input fails
 * closed to 0.
 */
export function clampScrollTopToOffsets(
  scrollTop: number,
  offsets: readonly number[],
  viewportHeight: number
): number {
  if (!Number.isFinite(scrollTop)) return 0;
  const safeScrollTop = Math.max(0, scrollTop);
  const total = totalRowHeight(offsets);
  if (total <= 0) return 0;
  if (!Number.isFinite(viewportHeight) || viewportHeight < 0) return 0;
  return Math.min(safeScrollTop, Math.max(0, total - viewportHeight));
}

/**
 * The band of rows to render for a given scroll position.
 *
 * Covers everything intersecting `[scrollTop, scrollTop + viewportHeight)`
 * plus `overscan` rows on each side, so scrolling never exposes an unpainted
 * edge. The overscan is counted in ROWS, not pixels, matching
 * `computeWindow`'s contract and `BRANCH_OVERSCAN`.
 *
 * Degenerate geometry paints nothing, exactly as the fixed-height version
 * does — except for the last-line guarantee folded in at the end: if
 * renderable content exists but the band came out empty (an anchor past the
 * content, a float round-trip at the very bottom), one real row is painted
 * instead of a blank pane. The fixed-height module needs a second function,
 * `ensureNonEmptyWindow`, for this; here it is part of the same answer
 * because there is no other caller who would want the empty band.
 */
export function windowFromOffsets(
  scrollTop: number,
  viewportHeight: number,
  offsets: readonly number[],
  overscan: number
): RowWindow {
  const total = rowCount(offsets);
  if (total <= 0) return { start: 0, end: 0 };
  if (!Number.isFinite(scrollTop)) {
    // An unknown scroll anchor paints nothing instead of guessing the top.
    return { start: 0, end: 0 };
  }
  const safeScrollTop = Math.max(0, scrollTop);
  const scan =
    overscan === Number.POSITIVE_INFINITY
      ? total
      : Number.isFinite(overscan)
        ? Math.max(0, Math.floor(overscan))
        : 0;

  // First row whose BOTTOM is past the scroll top, i.e. the first row with
  // any pixel on screen. Zero-height rows plateau the prefix array and are
  // skipped, which is correct — they occupy no pixels.
  const firstVisible = Math.min(firstOffsetAbove(offsets, safeScrollTop) - 1, total);

  const hasViewport = Number.isFinite(viewportHeight) && viewportHeight > 0;
  // First row starting at or after the viewport bottom: one past the last
  // row with a pixel on screen.
  const lastVisibleExclusive = hasViewport
    ? firstOffsetAtLeast(offsets, safeScrollTop + viewportHeight)
    : firstVisible;

  const start = Math.max(0, Math.min(firstVisible - scan, total));
  const end = Math.max(start, Math.min(total, lastVisibleExclusive + scan));

  if (end > start || !hasViewport) return { start, end };
  // Last-line guarantee: content exists and the viewport is measured, but the
  // band collapsed. Paint the row the anchor landed on.
  const anchored = Math.max(0, Math.min(start, total - 1));
  return { start: anchored, end: anchored + 1 };
}

/**
 * Scroll offset that brings row `index` fully into view, or `null` when it
 * already is and no scroll is needed.
 *
 * Mirrors the minimal-movement behaviour the sidebar's keyboard navigation
 * has always had: a row above the viewport scrolls to its top edge, a row
 * below scrolls so its bottom edge sits at the viewport bottom, and a row
 * already inside does not move the list at all. Returning `null` rather than
 * the current `scrollTop` keeps "no movement" distinguishable from "move to
 * exactly here", so the caller never issues a redundant smooth scroll.
 */
export function scrollOffsetToReveal(
  offsets: readonly number[],
  index: number,
  scrollTop: number,
  viewportHeight: number
): number | null {
  if (!Number.isInteger(index) || index < 0 || index >= rowCount(offsets)) return null;
  if (!Number.isFinite(scrollTop) || !Number.isFinite(viewportHeight) || viewportHeight <= 0) {
    return null;
  }
  const safeScrollTop = Math.max(0, scrollTop);
  const top = rowTop(offsets, index);
  const bottom = rowBottom(offsets, index);
  if (top < safeScrollTop) return top;
  if (bottom > safeScrollTop + viewportHeight) return bottom - viewportHeight;
  return null;
}

/**
 * Scroll offset that parks row `index` about a third of the way down the
 * viewport — the "locate this branch" gesture, where the point is to show
 * the row in context rather than flush against an edge.
 *
 * Clamped into the scrollable range so locating the first or last row does
 * not ask the browser for an offset it will silently refuse.
 */
export function scrollOffsetToCenter(
  offsets: readonly number[],
  index: number,
  viewportHeight: number
): number {
  const top = rowTop(offsets, index);
  const bias = Number.isFinite(viewportHeight) && viewportHeight > 0 ? viewportHeight / 3 : 0;
  return clampScrollTopToOffsets(Math.max(0, top - bias), offsets, viewportHeight);
}
