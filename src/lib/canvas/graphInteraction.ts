export type TooltipPlacement = "above" | "below";

export interface GraphTooltipPosition {
  left: number;
  top: number;
  placement: TooltipPlacement;
  /**
   * X of the pointer inside the tooltip box — where the caret belongs so it
   * keeps pointing at the cursor even after horizontal clamping shoves the
   * box away from the pointer. Always within [16, width-16] when the box is
   * at least 32px wide; callers render the caret at this offset instead of a
   * fixed left edge that silently detaches near the right border.
   */
  anchorX: number;
}

export interface VerticalScroller {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

export interface GraphGutterScroller {
  scrollLeft: number;
  scrollWidth: number;
  clientWidth: number;
}

export interface GraphWheelGesture {
  ctrlKey: boolean;
  shiftKey: boolean;
  deltaX: number;
  deltaY: number;
  deltaMode: number;
}

/** Visible graph gutter box plus its overflow pan, in CSS pixels. */
export interface GraphPointerViewport {
  left: number;
  top: number;
  scrollLeft: number;
}

/** Distance before a gutter press becomes a pan instead of a node click. */
export const GRAPH_PAN_THRESHOLD_PX = 5;

const TOOLTIP_EDGE_GAP = 8;
const TOOLTIP_POINTER_GAP = 12;

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

/**
 * Converts WheelEvent deltas into CSS pixels before forwarding them from the
 * canvas gutter to the commit-list scroller.
 */
export function normalizeWheelDelta(
  deltaY: number,
  deltaMode: number,
  rowHeight: number,
  viewportHeight: number,
): number {
  if (!Number.isFinite(deltaY)) return 0;
  if (deltaMode === 1) return deltaY * Math.max(1, rowHeight);
  if (deltaMode === 2) return deltaY * Math.max(1, viewportHeight);
  return deltaY;
}

/** Applies a canvas wheel gesture to the commit list and reports whether it moved. */
export function forwardGraphWheel(
  scroller: VerticalScroller,
  deltaY: number,
  deltaMode: number,
  rowHeight: number,
): boolean {
  const delta = normalizeWheelDelta(
    deltaY,
    deltaMode,
    rowHeight,
    scroller.clientHeight,
  );
  if (delta === 0) return false;

  const maxScrollTop = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
  const nextScrollTop = clamp(scroller.scrollTop + delta, 0, maxScrollTop);
  if (nextScrollTop === scroller.scrollTop) return false;
  scroller.scrollTop = nextScrollTop;
  return true;
}

function scrollerMaxX(scroller: GraphGutterScroller): number {
  const width = Number.isFinite(scroller.scrollWidth) ? scroller.scrollWidth : 0;
  const view = Number.isFinite(scroller.clientWidth) ? scroller.clientWidth : 0;
  return Math.max(0, width - view);
}

/** Pans extra branch lanes inside the capped graph gutter. */
export function panGraphHorizontally(
  scroller: GraphGutterScroller,
  deltaX: number,
): boolean {
  if (!Number.isFinite(deltaX) || deltaX === 0) return false;
  const max = scrollerMaxX(scroller);
  if (max <= 0) return false;
  const current = Number.isFinite(scroller.scrollLeft) ? scroller.scrollLeft : 0;
  const next = clamp(current + deltaX, 0, max);
  if (next === current) return false;
  scroller.scrollLeft = next;
  return true;
}

/**
 * Consumes a wheel gesture over the graph gutter.
 *
 * Shift+wheel and dominant deltaX pan extra lanes; otherwise the delta is
 * forwarded to the commit list so history stays linked to the canvas.
 * Pinch-zoom (ctrl+wheel) is consumed so the webview cannot scale the app.
 * Returns whether the caller should preventDefault.
 */
export function applyGraphGutterWheel(
  event: GraphWheelGesture,
  gutter: GraphGutterScroller,
  list: VerticalScroller,
  rowHeight: number,
): boolean {
  if (event.ctrlKey) return true;
  const canScrollX = scrollerMaxX(gutter) > 0.5;
  const dx = Number.isFinite(event.deltaX) ? event.deltaX : 0;
  const dy = Number.isFinite(event.deltaY) ? event.deltaY : 0;
  if (canScrollX && event.shiftKey) {
    panGraphHorizontally(gutter, dy);
    return true;
  }
  if (canScrollX && Math.abs(dx) > Math.abs(dy)) {
    panGraphHorizontally(gutter, dx);
    return true;
  }
  return forwardGraphWheel(list, dy, event.deltaMode, rowHeight);
}

export function graphDragScrollLeft(
  startScrollLeft: number,
  pointerStartX: number,
  pointerX: number,
): number {
  const start = Number.isFinite(startScrollLeft) ? startScrollLeft : 0;
  const from = Number.isFinite(pointerStartX) ? pointerStartX : 0;
  const to = Number.isFinite(pointerX) ? pointerX : 0;
  return start - (to - from);
}

export function isGraphPanGesture(
  startX: number,
  startY: number,
  x: number,
  y: number,
): boolean {
  const dx = (Number.isFinite(x) ? x : 0) - (Number.isFinite(startX) ? startX : 0);
  const dy = (Number.isFinite(y) ? y : 0) - (Number.isFinite(startY) ? startY : 0);
  return dx * dx + dy * dy >= GRAPH_PAN_THRESHOLD_PX * GRAPH_PAN_THRESHOLD_PX;
}

/**
 * Maps a pointer from client space onto the graph canvas.
 *
 * The gutter viewport can pan horizontally (extra branch lanes). Content X is
 * `clientX - viewport.left + scrollLeft`; using a cached canvas rect instead
 * misses every node that only became visible after the pan.
 */
export function canvasPointFromClient(
  clientX: number,
  clientY: number,
  viewport: GraphPointerViewport,
): { x: number; y: number } {
  const left = Number.isFinite(viewport.left) ? viewport.left : 0;
  const top = Number.isFinite(viewport.top) ? viewport.top : 0;
  const scrollLeft = Number.isFinite(viewport.scrollLeft) ? viewport.scrollLeft : 0;
  const x = (Number.isFinite(clientX) ? clientX : 0) - left + scrollLeft;
  const y = (Number.isFinite(clientY) ? clientY : 0) - top;
  return {
    x: Number.isFinite(x) ? x : 0,
    y: Number.isFinite(y) ? y : 0,
  };
}

/** Positions a graph-node tooltip without letting it escape the visible pane. */
export function positionGraphTooltip(
  pointerX: number,
  pointerY: number,
  viewportWidth: number,
  viewportHeight: number,
  tooltipWidth: number,
  tooltipHeight: number,
): GraphTooltipPosition {
  // A NaN pointer used to flow through clamp() and freeze the tooltip at a
  // translate3d(NaN) position; sibling normalizeWheelDelta already guards,
  // so mirror it here for parity.
  if (
    !Number.isFinite(pointerX) ||
    !Number.isFinite(pointerY) ||
    !Number.isFinite(viewportWidth) ||
    !Number.isFinite(viewportHeight) ||
    !Number.isFinite(tooltipWidth) ||
    !Number.isFinite(tooltipHeight)
  ) {
    return { left: TOOLTIP_EDGE_GAP, top: TOOLTIP_EDGE_GAP, placement: "below", anchorX: 16 };
  }
  const maxLeft = Math.max(TOOLTIP_EDGE_GAP, viewportWidth - tooltipWidth - TOOLTIP_EDGE_GAP);
  const left = clamp(
    pointerX + TOOLTIP_POINTER_GAP,
    TOOLTIP_EDGE_GAP,
    maxLeft,
  );
  const fitsBelow =
    pointerY + TOOLTIP_POINTER_GAP + tooltipHeight <= viewportHeight - TOOLTIP_EDGE_GAP;
  const placement: TooltipPlacement = fitsBelow ? "below" : "above";
  const preferredTop = fitsBelow
    ? pointerY + TOOLTIP_POINTER_GAP
    : pointerY - TOOLTIP_POINTER_GAP - tooltipHeight;
  const maxTop = Math.max(TOOLTIP_EDGE_GAP, viewportHeight - tooltipHeight - TOOLTIP_EDGE_GAP);
  // Caret tracks the pointer within the clamped box; inset keeps the rotated
  // square (12px) fully inside the rounded corner radius at both ends.
  const anchorX = clamp(pointerX - left, 16, Math.max(16, tooltipWidth - 16));

  return {
    left,
    top: clamp(preferredTop, TOOLTIP_EDGE_GAP, maxTop),
    placement,
    anchorX,
  };
}
