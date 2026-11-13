/**
 * Height geometry and drag plumbing for resizable bottom panes.
 *
 * The terminal dock and the commit details pane both sit under a view, grow
 * when their top edge is dragged up, nudge with ArrowUp/ArrowDown and persist
 * one height each. The clamp, the ceiling against the container and the
 * pointer-capture drag have one owner here so the two cannot drift apart;
 * each pane supplies only its bounds and what it does with a height.
 */

export interface PaneBounds {
  min: number;
  max: number;
  /** What a non-finite request becomes. */
  fallback: number;
}

/** The commit details pane under the History list (it was a fixed `h-64`). */
export const COMMIT_PANE_BOUNDS: PaneBounds = { min: 140, max: 1600, fallback: 256 };
/** Keyboard resize step for the commit pane separator. */
export const COMMIT_PANE_RESIZE_STEP = 24;
/** The commit list above the pane always keeps at least this much height. */
export const COMMIT_PANE_MIN_VIEW_HEIGHT = 160;

/**
 * Clamp a requested pane height to its bounds.
 *
 * Fail-closed on hostile input: a non-finite value falls back rather than
 * poisoning a persisted preference (a stored `NaN` would render a zero-pixel
 * pane with no separator left to grab). Whole pixels: fractional heights from
 * sub-pixel drag deltas make the flex layout shimmer between paints.
 */
export function clampPaneHeight(px: number, bounds: PaneBounds): number {
  if (!Number.isFinite(px)) return bounds.fallback;
  if (px < bounds.min) return bounds.min;
  if (px > bounds.max) return bounds.max;
  return Math.round(px);
}

/**
 * The largest pane that still leaves the view above it `minViewHeight`.
 *
 * An unmeasured container (zero or non-finite) means "unknown", not "no
 * room", and returns the clamped request. A container too short for both
 * returns the minimum: a pane shrunk to nothing cannot be dragged back.
 */
export function fitPaneHeight(
  requested: number,
  containerHeight: number,
  minViewHeight: number,
  bounds: PaneBounds,
): number {
  const clamped = clampPaneHeight(requested, bounds);
  if (!Number.isFinite(containerHeight) || containerHeight <= 0) return clamped;
  const ceiling = containerHeight - minViewHeight;
  if (ceiling < bounds.min) return bounds.min;
  return Math.min(clamped, Math.round(ceiling));
}

/**
 * The height an ArrowUp / ArrowDown asks for, or null for any other key.
 * Up grows the pane, matching a drag upward.
 */
export function nudgedPaneHeight(key: string, current: number, step: number): number | null {
  if (key === "ArrowUp") return current + step;
  if (key === "ArrowDown") return current - step;
  return null;
}

export interface PaneDragHandlers {
  /** The clamped height the pointer is asking for; called per pointermove. */
  onHeight: (px: number) => void;
  /** The drag finished (pointerup or pointercancel); commit what was shown. */
  onEnd: () => void;
}

/**
 * Begin dragging a pane's top-edge handle from a pointerdown.
 *
 * Pointer capture, not window listeners: a drag that leaves the window still
 * ends on pointerup, and nothing else on the page sees the moves. Dragging up
 * grows the pane, so the delta is inverted, and it is clamped exactly as the
 * store would clamp it so the release commits what was shown. Returns false
 * (and does nothing) for a non-primary button.
 */
export function startPaneDrag(
  event: PointerEvent,
  startHeight: number,
  bounds: PaneBounds,
  handlers: PaneDragHandlers,
): boolean {
  if (event.button !== 0) return false;
  event.preventDefault();
  const startY = event.clientY;
  const handle = event.currentTarget as HTMLElement;
  handle.setPointerCapture(event.pointerId);

  const move = (moveEvent: PointerEvent) => {
    handlers.onHeight(clampPaneHeight(startHeight + (startY - moveEvent.clientY), bounds));
  };
  const end = () => {
    handle.removeEventListener("pointermove", move);
    handle.removeEventListener("pointerup", end);
    handle.removeEventListener("pointercancel", end);
    handle.releasePointerCapture(event.pointerId);
    handlers.onEnd();
  };
  handle.addEventListener("pointermove", move);
  handle.addEventListener("pointerup", end);
  handle.addEventListener("pointercancel", end);
  return true;
}
