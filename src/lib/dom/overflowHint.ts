/**
 * Overflow advertisement for scrollers that do not show a reliable scrollbar
 * (overlay scrollbars, `scrollbar-width: none` chrome, capped graph gutters).
 *
 * Geometry is owned here so every surface — repo tabs, the graph, the
 * sidebar, the file explorer — answers the same question: is there more
 * content past this edge? The graph gutter used to compute that privately
 * for its fade; the fade is now one presentation of this hint, and the
 * chevron on the overflowing edge is the other.
 */

import { createResizeObservation } from "./observeResize";

export type OverflowAxis = "x" | "y";

export interface OverflowHint {
  canScroll: boolean;
  /** More content toward the start (left / top). */
  showStart: boolean;
  /** More content toward the end (right / bottom). */
  showEnd: boolean;
}

export const EMPTY_OVERFLOW_HINT: OverflowHint = {
  canScroll: false,
  showStart: false,
  showEnd: false,
};

/** Sub-pixel leftover that is not worth advertising as overflow. */
const SCROLL_EPS = 0.5;

/**
 * Pixels from an edge before that edge's cue appears. A 1px remainder is
 * treated as "already there" so the cue does not flicker against overlay
 * scrollbar subpixels. For a gutter shorter than 2px the threshold shrinks
 * so a real-but-tiny overflow still shows the end arrow.
 */
function edgeThreshold(max: number): number {
  if (!(max > 0)) return 0;
  return Math.min(1, max / 2);
}

function finiteNonNegative(value: number, fallback = 0): number {
  return Number.isFinite(value) && value >= 0 ? value : fallback;
}

export function resolveOverflowHint(
  scrollPos: number,
  viewport: number,
  content: number,
): OverflowHint {
  const view = finiteNonNegative(viewport);
  const size = finiteNonNegative(content);
  const max = Math.max(0, size - view);
  const pos = Number.isFinite(scrollPos)
    ? Math.min(max, Math.max(0, scrollPos))
    : 0;
  const canScroll = max > SCROLL_EPS;
  if (!canScroll) return EMPTY_OVERFLOW_HINT;
  const edge = edgeThreshold(max);
  return {
    canScroll: true,
    showStart: pos > edge,
    showEnd: pos < max - edge,
  };
}

export interface OverflowBox {
  scrollLeft: number;
  scrollTop: number;
  clientWidth: number;
  clientHeight: number;
  scrollWidth: number;
  scrollHeight: number;
}

export function readOverflowHint(el: OverflowBox, axis: OverflowAxis): OverflowHint {
  if (axis === "x") {
    return resolveOverflowHint(el.scrollLeft, el.clientWidth, el.scrollWidth);
  }
  return resolveOverflowHint(el.scrollTop, el.clientHeight, el.scrollHeight);
}

/** Distance a cue click pans, in the scroller's own pixels. */
export function overflowNudge(viewport: number, toward: "start" | "end"): number {
  const view = finiteNonNegative(viewport);
  const magnitude = Math.max(48, view * 0.8);
  return toward === "end" ? magnitude : -magnitude;
}

export interface OverflowScroller extends OverflowBox {
  scrollBy(options: ScrollToOptions): void;
}

export function scrollOverflowBy(
  el: OverflowScroller,
  axis: OverflowAxis,
  toward: "start" | "end",
  options?: { behavior?: ScrollBehavior },
): void {
  const viewport = axis === "x" ? el.clientWidth : el.clientHeight;
  const delta = overflowNudge(viewport, toward);
  el.scrollBy({
    left: axis === "x" ? delta : 0,
    top: axis === "y" ? delta : 0,
    behavior: options?.behavior ?? "auto",
  });
}

/**
 * Map a vertical wheel onto a horizontal scroller.
 *
 * Trackpads and mice report vertical deltas even when the pointer is over a
 * row that only scrolls on X. Returning null leaves the event to the browser
 * (already-horizontal wheels, or a strip that does not overflow).
 */
export function verticalWheelToHorizontalDelta(event: {
  deltaX: number;
  deltaY: number;
}): number | null {
  if (!Number.isFinite(event.deltaX) || !Number.isFinite(event.deltaY)) return null;
  if (Math.abs(event.deltaX) >= Math.abs(event.deltaY)) return null;
  if (event.deltaY === 0) return null;
  return event.deltaY;
}

export function applyHorizontalScrollDelta(
  el: { scrollLeft: number; scrollWidth: number; clientWidth: number },
  delta: number,
): number {
  const view = finiteNonNegative(el.clientWidth);
  const size = finiteNonNegative(el.scrollWidth);
  const max = Math.max(0, size - view);
  const pos = Number.isFinite(el.scrollLeft) ? el.scrollLeft : 0;
  if (!Number.isFinite(delta) || max <= SCROLL_EPS) return pos;
  return Math.min(max, Math.max(0, pos + delta));
}

/**
 * The smallest scrollLeft that keeps `child` fully inside the scroller.
 * `scrollIntoView` can pan a parent instead of this strip; this cannot.
 */
export function scrollChildIntoHorizontalView(
  scroller: { scrollLeft: number; clientWidth: number; scrollWidth: number },
  child: { offsetLeft: number; offsetWidth: number },
): number {
  const view = finiteNonNegative(scroller.clientWidth);
  const size = finiteNonNegative(scroller.scrollWidth);
  const max = Math.max(0, size - view);
  const left = finiteNonNegative(child.offsetLeft);
  const width = finiteNonNegative(child.offsetWidth);
  const right = left + width;
  const start = Number.isFinite(scroller.scrollLeft) ? scroller.scrollLeft : 0;
  if (view <= 0) return start;
  if (left < start) return Math.min(max, left);
  if (right > start + view) return Math.min(max, Math.max(0, right - view));
  return Math.min(max, Math.max(0, start));
}

/**
 * Live overflow for a DOM scroller. Resize of the scroller or its direct
 * children, plus childList mutations (virtual-list spacers), all retrigger
 * the same read. Scroll is coalesced to animation frames so a trackpad
 * fling is one hint update, not one per event. Resize deliveries go through
 * observeResize so they never write during the browser's observer turn.
 */
export function observeOverflow(
  el: HTMLElement,
  axis: OverflowAxis,
  onChange: (hint: OverflowHint) => void,
): () => void {
  let frame = 0;
  const notify = () => {
    if (frame) cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => {
      frame = 0;
      onChange(readOverflowHint(el, axis));
    });
  };
  notify();
  el.addEventListener("scroll", notify, { passive: true });
  const resize = createResizeObservation(() => {
    onChange(readOverflowHint(el, axis));
  });
  resize.observe(el);
  const watchChildren = () => {
    for (const child of el.children) {
      resize.observe(child);
    }
  };
  watchChildren();
  const mo = new MutationObserver(() => {
    watchChildren();
    notify();
  });
  mo.observe(el, { childList: true });
  return () => {
    if (frame) cancelAnimationFrame(frame);
    el.removeEventListener("scroll", notify);
    resize.disconnect();
    mo.disconnect();
  };
}
