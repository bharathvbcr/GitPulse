/**
 * How a terminal's history scrolls: the options GitPulse sets on xterm for
 * wheel feel, the "scrolled back" test the Latest-output button uses, a
 * per-event wheel-notch classifier, and a guard for one race smooth scrolling
 * opens.
 *
 * ## What xterm does (6.1.0-beta.317, read in node_modules/@xterm/xterm/src)
 *
 *  - `smoothScrollDuration` defaults to 0 (`common/services/OptionsService.ts`),
 *    so a mouse-wheel notch moves the viewport by whole rows at once: 100 px of
 *    `deltaY` is 2.5 ticks of 50 px (`scrollableElement.ts`, `SCROLL_WHEEL_SENSITIVITY`),
 *    about 8 rows at this font size.
 *  - With a duration set, only events xterm's own `MouseWheelClassifier` takes
 *    for a physical wheel are animated (`isPhysicalMouseWheel`, `_handleMouseWheel`).
 *    That classifier scores whole-number deltas as a wheel, and a trackpad can
 *    send whole numbers, so GitPulse does not rely on it alone: `isWheelNotch`
 *    decides per event and the duration is 0 while the gesture is not a notch.
 *    xterm derives the delta it scrolls by from `wheelDeltaY / 120` when the
 *    event has it (`mouseEvent.ts`, `StandardWheelEvent`), so a notch is an
 *    event whose `wheelDeltaY` is a non-zero multiple of 120, or whose
 *    `deltaMode` is lines.
 *  - The viewport is row-quantized: `Viewport._handleScroll` rounds the
 *    animated pixel position to a row, so "smooth" means the same distance in
 *    smaller row steps, not sub-row motion.
 *
 * ## The race, and the guard
 *
 * While the view sits at the bottom, output carries it along (`ydisp` follows
 * `ybase`), because the buffer only counts the user as scrolling once a whole
 * row of scroll has been delivered to it (`BufferService.scrollLines` sets
 * `isUserScrolling`, reached from `Viewport._handleScroll`'s row diff). A smooth
 * wheel animation starts slowly, so its first frames round to zero rows; if a
 * line of output lands in that window, `Viewport._sync` sees `ydisp` differ from
 * the animation's row, treats that as another component moving the view,
 * cancels the animation and snaps to the output. The gesture is swallowed and
 * the view stays at the bottom: scrolling back while output streams would not
 * scroll back. Measured with the bench: docs/PERFORMANCE.md (2026-11-06).
 * The guard closes it, and only for events xterm would animate (a notch): on
 * the first such event at the bottom it delivers one row at once, before
 * xterm's own handler runs. The buffer is then scrolled back, output no longer
 * moves it, and the animation continues from there.
 *
 * `Terminal.scrollLines` goes through the viewport, which animates whenever the
 * duration is not 0, and xterm has no public way to scroll without it
 * (`Viewport.scrollToLine(line, true)` is internal), so the guard writes the
 * option: 0 for the one call, then back. Each write refreshes every row
 * (and the DOM renderer rewrites its CSS); the bench measures that cost
 * (docs/PERFORMANCE.md) and the writes happen at most twice per gesture that
 * starts at the bottom and once when the device changes, never per event.
 */

/** How long one wheel notch takes to glide, in ms. Chosen by the sweep in docs/PERFORMANCE.md. */
export const SMOOTH_SCROLL_MS = 150;
/** One notch in `wheelDeltaY` units (`mouseEvent.ts` divides by it). */
export const WHEEL_NOTCH_UNITS = 120;
/** A pause this long ends a gesture; the next event is judged afresh. */
export const GESTURE_GAP_MS = 250;
/** Below this many pixels an event is noise, never a notch (a trackpad's resting jitter). */
export const MIN_WHEEL_DELTA = 1;
/** Samples the opt-in wheel log keeps. */
export const WHEEL_LOG_LIMIT = 50;

const REDUCED_MOTION = "(prefers-reduced-motion: reduce)";

/** The person asked the system for less motion. */
export function prefersReducedMotion(): boolean {
  try {
    return typeof globalThis.matchMedia === "function" && globalThis.matchMedia(REDUCED_MOTION).matches === true;
  } catch {
    return false;
  }
}

/** The glide duration when nothing else is known about the input: 0 if the glide is off or motion is reduced. */
export function glideDuration(enabled: boolean = true): number {
  return enabled && !prefersReducedMotion() ? SMOOTH_SCROLL_MS : 0;
}

/** The xterm options this module owns. */
export function scrollFeelOptions(enabled: boolean = true): { smoothScrollDuration: number } {
  return { smoothScrollDuration: glideDuration(enabled) };
}

export interface ScrollBuffer {
  readonly viewportY: number;
  readonly baseY: number;
  readonly type?: "normal" | "alternate";
}

/** The view is above the newest output: the "Latest output" button shows. */
export function isScrolledBack(buffer: ScrollBuffer): boolean {
  return buffer.viewportY < buffer.baseY;
}

export interface WheelLike {
  readonly deltaY: number;
  readonly deltaMode?: number;
  /** Chromium and WebKit's legacy field, which xterm reads before `deltaY`. */
  readonly wheelDeltaY?: number;
  readonly timeStamp?: number;
}

const DOM_DELTA_LINE = 1;

/**
 * Whether one event is a wheel notch, the only input xterm should animate.
 * A trackpad's deltas are fractional or not multiples of a notch, and its
 * resting jitter is under a pixel.
 */
export function isWheelNotch(event: WheelLike): boolean {
  if (!Number.isFinite(event.deltaY) || Math.abs(event.deltaY) < MIN_WHEEL_DELTA) return false;
  if (event.deltaMode === DOM_DELTA_LINE) return true;
  const legacy = event.wheelDeltaY;
  if (typeof legacy === "number") return legacy !== 0 && Math.abs(legacy) % WHEEL_NOTCH_UNITS === 0;
  // No legacy field (Gecko): xterm falls back to `deltaY / 40`; a whole number is the best signal left.
  return Number.isInteger(event.deltaY);
}

export interface WheelSample {
  readonly at: number;
  readonly deltaY: number;
  readonly wheelDeltaY: number | null;
  readonly deltaMode: number;
  /** The event on its own. */
  readonly notch: boolean;
  /** What the gesture was taken for: the glide applies only when this is true. */
  readonly gestureNotch: boolean;
}

/**
 * Judges events in gestures. The first event after a pause decides the class
 * of the gesture; a later event that is not a notch ends the notch class for
 * the rest of it (a trackpad's whole-number frames among its fractional ones
 * must not keep a glide on). Deciding per gesture, not per event, is what
 * keeps the duration from toggling mid-scroll.
 */
export function createGestureClassifier(gapMs: number = GESTURE_GAP_MS): (event: WheelLike, at: number) => WheelSample {
  let last = Number.NEGATIVE_INFINITY;
  let gesture = false;
  return (event, at) => {
    const notch = isWheelNotch(event);
    if (at - last > gapMs) gesture = notch;
    else if (!notch) gesture = false;
    last = at;
    return {
      at,
      deltaY: event.deltaY,
      wheelDeltaY: typeof event.wheelDeltaY === "number" ? event.wheelDeltaY : null,
      deltaMode: event.deltaMode ?? 0,
      notch,
      gestureNotch: gesture,
    };
  };
}

/** The last `limit` wheel samples, for the opt-in diagnostic. */
export function createWheelLog(limit: number = WHEEL_LOG_LIMIT): { push(sample: WheelSample): void; samples(): WheelSample[] } {
  const kept: WheelSample[] = [];
  return {
    push(sample) {
      kept.push(sample);
      if (kept.length > limit) kept.shift();
    },
    samples: () => [...kept],
  };
}

export interface ScrollGuardTerminal {
  readonly element?: HTMLElement;
  options: { smoothScrollDuration?: number };
  readonly buffer: { readonly active: ScrollBuffer };
  readonly modes: { readonly mouseTrackingMode: string };
  scrollLines(amount: number): void;
}

/**
 * True when a wheel event heading toward history should move the view off the
 * bottom now: it is pinned there, there is history, the program is not
 * reading the wheel itself (mouse tracking), and this is the normal buffer.
 */
export function shouldLeaveBottom(deltaY: number, terminal: Pick<ScrollGuardTerminal, "buffer" | "modes">): boolean {
  const buffer = terminal.buffer.active;
  return deltaY < 0
    && buffer.type !== "alternate"
    && terminal.modes.mouseTrackingMode === "none"
    && buffer.baseY > 0
    && !isScrolledBack(buffer);
}

export interface ScrollFeelConfig {
  /** The "Smooth mouse-wheel scrolling" setting. Read on every event. */
  enabled?: () => boolean;
  /** Where each wheel sample goes (the opt-in diagnostic). */
  onSample?: (sample: WheelSample) => void;
}

export interface ScrollFeel {
  /** Re-applies the duration after the setting or the system's motion preference changed. */
  refresh(): void;
  detach(): void;
}

/**
 * Sets the glide for notches only, guards the first notch at the bottom, and
 * follows the reduced-motion preference. A terminal that is not open yet has
 * nothing to guard.
 */
export function createScrollFeel(terminal: ScrollGuardTerminal, config: ScrollFeelConfig = {}): ScrollFeel {
  const element = terminal.element;
  if (!element) return { refresh: () => {}, detach: () => {} };
  const enabled = config.enabled ?? (() => true);
  const classify = createGestureClassifier();
  let gestureNotch = true;
  const wanted = () => (gestureNotch ? glideDuration(enabled()) : 0);
  const apply = (duration: number) => {
    if (terminal.options.smoothScrollDuration !== duration) terminal.options.smoothScrollDuration = duration;
  };
  const onWheel = (event: WheelEvent) => {
    const sample = classify(event, event.timeStamp ?? Date.now());
    config.onSample?.(sample);
    gestureNotch = sample.gestureNotch;
    if (sample.notch && wanted() > 0 && shouldLeaveBottom(event.deltaY, terminal)) {
      const duration = terminal.options.smoothScrollDuration;
      if (duration) terminal.options.smoothScrollDuration = 0;
      try {
        terminal.scrollLines(-1);
      } finally {
        if (duration) terminal.options.smoothScrollDuration = duration;
      }
    }
    apply(wanted());
  };
  // Capture: before xterm's listener on its scrollable element reads the position.
  element.addEventListener("wheel", onWheel, { capture: true, passive: true });
  let media: MediaQueryList | null = null;
  const onMotionChange = () => apply(wanted());
  try {
    media = typeof globalThis.matchMedia === "function" ? globalThis.matchMedia(REDUCED_MOTION) : null;
    if (typeof media?.addEventListener === "function") media.addEventListener("change", onMotionChange);
  } catch {
    media = null;
  }
  return {
    refresh: () => apply(wanted()),
    detach: () => {
      element.removeEventListener("wheel", onWheel, { capture: true });
      if (typeof media?.removeEventListener === "function") media.removeEventListener("change", onMotionChange);
    },
  };
}

/** `createScrollFeel` for a caller that only needs to take it down again. */
export function attachScrollGuard(terminal: ScrollGuardTerminal, config: ScrollFeelConfig = {}): () => void {
  return createScrollFeel(terminal, config).detach;
}
