export type TooltipPlacement = "above" | "below";

export interface GraphTooltipPosition {
  left: number;
  top: number;
  placement: TooltipPlacement;
}

export interface VerticalScroller {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

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

/** Positions a graph-node tooltip without letting it escape the visible pane. */
export function positionGraphTooltip(
  pointerX: number,
  pointerY: number,
  viewportWidth: number,
  viewportHeight: number,
  tooltipWidth: number,
  tooltipHeight: number,
): GraphTooltipPosition {
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

  return {
    left,
    top: clamp(preferredTop, TOOLTIP_EDGE_GAP, maxTop),
    placement,
  };
}
