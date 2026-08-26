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
