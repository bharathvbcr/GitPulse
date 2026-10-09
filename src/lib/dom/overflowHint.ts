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

import { createResizeObservation, type ObserveResizeOptions } from "./observeResize";

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

/**
 * Scroll extent at each end that is not content: padding, and the rounding
 * slack of integer `scrollWidth`/`clientWidth` against fractional layout.
 * Hiding only this much is not hiding anything, so it never earns a cue.
 */
export interface OverflowInsets {
  start: number;
  end: number;
}

export const NO_OVERFLOW_INSETS: OverflowInsets = { start: 0, end: 0 };

export function resolveOverflowHint(
  scrollPos: number,
  viewport: number,
  content: number,
  insets: OverflowInsets = NO_OVERFLOW_INSETS,
): OverflowHint {
  const view = finiteNonNegative(viewport);
  const size = finiteNonNegative(content);
  const max = Math.max(0, size - view);
  const pos = Number.isFinite(scrollPos)
    ? Math.min(max, Math.max(0, scrollPos))
    : 0;
  if (!(max > SCROLL_EPS)) return EMPTY_OVERFLOW_HINT;
  // What is past each edge, less the padding that sits there: an edge that
  // hides only padding hides nothing. Padding at the other edge is in view
  // and does not count -- a toolbar scrolled to 0 still hides its last
  // control even when the controls alone would fit.
  const edge = edgeThreshold(max);
  const showStart = pos - finiteNonNegative(insets.start) > edge;
  const showEnd = max - pos - finiteNonNegative(insets.end) > edge;
  if (!showStart && !showEnd) return EMPTY_OVERFLOW_HINT;
  return { canScroll: true, showStart, showEnd };
}

export interface OverflowBox {
  scrollLeft: number;
  scrollTop: number;
  clientWidth: number;
  clientHeight: number;
  scrollWidth: number;
  scrollHeight: number;
}

export function readOverflowHint(
  el: OverflowBox,
  axis: OverflowAxis,
  insets: OverflowInsets = NO_OVERFLOW_INSETS,
): OverflowHint {
  if (axis === "x") {
    return resolveOverflowHint(el.scrollLeft, el.clientWidth, el.scrollWidth, insets);
  }
  return resolveOverflowHint(el.scrollTop, el.clientHeight, el.scrollHeight, insets);
}

/**
 * `scrollWidth` and `clientWidth` are rounded to whole pixels while layout is
 * fractional, so at a fractional zoom or device scale the two can disagree by
 * up to a pixel with nothing hidden at all.
 */
export const OVERFLOW_ROUNDING_SLACK = 0.5;

type InsetStyle = Pick<
  CSSStyleDeclaration,
  "paddingLeft" | "paddingRight" | "paddingTop" | "paddingBottom" | "borderLeftWidth" | "borderRightWidth" | "borderTopWidth" | "borderBottomWidth"
>;

function px(value: string): number {
  const n = Number.parseFloat(value);
  return Number.isFinite(n) && n > 0 ? n : 0;
}

/**
 * The non-content scroll extent of a DOM scroller: its own padding, plus the
 * padding and border of a single wrapping row (the `min-w-max px-3` toolbar
 * and title-bar shape), plus rounding slack. Only a lone child is unwrapped:
 * with several children the padding of one is not the edge of the content.
 */
export function readOverflowInsets(
  el: Element,
  axis: OverflowAxis,
  styleOf: (node: Element) => InsetStyle = (node) => getComputedStyle(node),
): OverflowInsets {
  const startPad = (style: InsetStyle) => (axis === "x" ? px(style.paddingLeft) : px(style.paddingTop));
  const endPad = (style: InsetStyle) => (axis === "x" ? px(style.paddingRight) : px(style.paddingBottom));
  const startBorder = (style: InsetStyle) => (axis === "x" ? px(style.borderLeftWidth) : px(style.borderTopWidth));
  const endBorder = (style: InsetStyle) => (axis === "x" ? px(style.borderRightWidth) : px(style.borderBottomWidth));
  const own = styleOf(el);
  let start = startPad(own) + OVERFLOW_ROUNDING_SLACK;
  let end = endPad(own) + OVERFLOW_ROUNDING_SLACK;
  const only = el.childElementCount === 1 ? el.firstElementChild : null;
  if (only) {
    const row = styleOf(only);
    start += startPad(row) + startBorder(row);
    end += endPad(row) + endBorder(row);
  }
  return { start, end };
}

/**
 * Which edges of a scroller fade, as the `data-gp-fade` token app.css masks.
 *
 * The fade is a mask on the scroller itself rather than a band painted over
 * it: a painted band needs a colour, and no one colour matches a glass title
 * bar, a surface toolbar and a base pane, so on most hosts it read as a
 * blank patch. A mask fades the content to whatever is behind it.
 */
export type OverflowFadeToken = `${OverflowAxis}-${"start" | "end" | "both"}`;

export function overflowFadeToken(
  axis: OverflowAxis,
  hint: OverflowHint,
): OverflowFadeToken | null {
  if (!hint.canScroll) return null;
  if (hint.showStart && hint.showEnd) return `${axis}-both`;
  if (hint.showStart) return `${axis}-start`;
  if (hint.showEnd) return `${axis}-end`;
  return null;
}

/** Write (or clear) the fade token; a repeated token leaves the DOM alone. */
export function setOverflowFade(
  el: { dataset: DOMStringMap },
  token: OverflowFadeToken | null,
): void {
  if (token === null) {
    if ("gpFade" in el.dataset) delete el.dataset.gpFade;
  } else if (el.dataset.gpFade !== token) {
    el.dataset.gpFade = token;
  }
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

export interface HorizontalWheelTarget {
  scrollLeft: number;
  scrollWidth: number;
  clientWidth: number;
  addEventListener(type: "wheel", listener: (event: WheelEvent) => void, options: AddEventListenerOptions): void;
  removeEventListener(type: "wheel", listener: (event: WheelEvent) => void): void;
}

/** Pixels per line for a line-mode wheel (Firefox, some Linux mice). */
export const WHEEL_LINE_PX = 40;

/**
 * A wheel delta in pixels. Line- and page-mode wheels report counts, not
 * pixels: three lines read as three pixels would barely move the strip.
 */
export function wheelDeltaPixels(delta: number, deltaMode: number, pagePx: number): number {
  if (!Number.isFinite(delta)) return 0;
  if (deltaMode === 1) return delta * WHEEL_LINE_PX;
  if (deltaMode === 2) return delta * Math.max(WHEEL_LINE_PX, finiteNonNegative(pagePx) * 0.9);
  return delta;
}

export type HorizontalWheelEvent = Pick<
  WheelEvent,
  "deltaX" | "deltaY" | "deltaMode" | "ctrlKey" | "metaKey" | "defaultPrevented" | "preventDefault"
>;

/**
 * Let a mouse wheel pan a strip that only scrolls on X. A strip that hides
 * its track is otherwise reachable only by trackpad, chevron or keyboard.
 *
 * The wheel is left to the page when the strip fits, when the strip is
 * already at the end the wheel points past (so scrolling chains on), when
 * it is a pinch or ctrl/cmd+wheel zoom, and when a nested handler already
 * claimed it.
 */
export function bindHorizontalWheel(el: HorizontalWheelTarget): () => void {
  const onWheel = (event: HorizontalWheelEvent) => {
    if (event.defaultPrevented || event.ctrlKey || event.metaKey) return;
    const raw = verticalWheelToHorizontalDelta(event);
    if (raw === null) return;
    if (el.scrollWidth <= el.clientWidth + SCROLL_EPS) return;
    const delta = wheelDeltaPixels(raw, event.deltaMode, el.clientWidth);
    const from = Number.isFinite(el.scrollLeft) ? el.scrollLeft : 0;
    const to = applyHorizontalScrollDelta(el, delta);
    if (Math.abs(to - from) < SCROLL_EPS) return;
    event.preventDefault();
    el.scrollLeft = to;
  };
  el.addEventListener("wheel", onWheel, { passive: false });
  return () => el.removeEventListener("wheel", onWheel);
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
  options: ObserveResizeOptions = {},
): () => void {
  let frame = 0;
  let disposed = false;
  // Insets are read with every answer rather than cached: a cached padding
  // went stale when a class swap changed it without resizing anything.
  const deliver = () => {
    if (!disposed) onChange(readOverflowHint(el, axis, readOverflowInsets(el, axis)));
  };
  const notify = () => {
    if (frame) cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => {
      frame = 0;
      deliver();
    });
  };
  el.addEventListener("scroll", notify, { passive: true });
  const resize = createResizeObservation(deliver, options);
  resize.observe(el);
  // Children are observed so a tab growing inside a fixed strip is seen, and
  // unobserved when they leave: a ResizeObserver holds its targets, so a strip
  // that lives all session would otherwise keep every closed tab alive.
  const watched = new Set<Element>();
  const watchChildren = () => {
    const current = new Set(el.children);
    for (const child of watched) {
      if (!current.has(child)) {
        resize.unobserve(child);
        watched.delete(child);
      }
    }
    for (const child of current) {
      if (!watched.has(child)) {
        // Border box: a row whose padding changes keeps its content box, and
        // the default content-box observation would never hear about it.
        resize.observe(child, { box: "border-box" });
        watched.add(child);
      }
    }
  };
  watchChildren();
  const mo = new MutationObserver(() => {
    // A late callback must not re-observe: observe() revives a disconnected
    // ResizeObserver, which would then hold these children for good.
    if (disposed) return;
    // The watched set changes now (it costs no layout); the measurement waits
    // for the frame, so a burst of tab opens is one read, not one per batch.
    watchChildren();
    notify();
  });
  mo.observe(el, { childList: true });
  // Synchronous first answer: a cue that moves to a new scroller must not
  // paint the previous scroller's edges for a frame while this one is read.
  deliver();
  return () => {
    disposed = true;
    if (frame) cancelAnimationFrame(frame);
    el.removeEventListener("scroll", notify);
    resize.disconnect();
    watched.clear();
    mo.disconnect();
  };
}
