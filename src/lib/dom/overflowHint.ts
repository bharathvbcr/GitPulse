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
 * Live overflow for a DOM scroller. Resize of the scroller or its direct
 * children, plus childList mutations (virtual-list spacers), all retrigger
 * the same read. Scroll is coalesced to animation frames so a trackpad
 * fling is one hint update, not one per event.
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
  const ro = new ResizeObserver(notify);
  ro.observe(el);
  const watchChildren = () => {
    for (const child of el.children) {
      ro.observe(child);
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
    ro.disconnect();
    mo.disconnect();
  };
}
