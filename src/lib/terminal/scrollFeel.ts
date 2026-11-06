/**
 * How a terminal's history scrolls: the options GitPulse sets on xterm for
 * wheel feel, the "scrolled back" test the Latest-output button uses, and a
 * guard for one race smooth scrolling opens.
 *
 * ## What xterm does (6.1.0-beta.317, read in node_modules/@xterm/xterm/src)
 *
 *  - `smoothScrollDuration` defaults to 0 (`common/services/OptionsService.ts`),
 *    so a mouse-wheel notch moves the viewport by whole rows at once: 100 px of
 *    `deltaY` is 2.5 ticks of 50 px (`scrollableElement.ts`, `SCROLL_WHEEL_SENSITIVITY`),
 *    about 8 rows at this font size.
 *  - With a duration set, only events xterm's `MouseWheelClassifier` takes for a
 *    physical wheel are animated (`isPhysicalMouseWheel`, `_handleMouseWheel`);
 *    everything else, a trackpad included, still moves directly. The
 *    classifier looks at the deltas alone, so GitPulse does not have to tell the
 *    devices apart.
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
 * `attachScrollGuard` closes it. On the first upward wheel event at the bottom
 * it delivers one row at once, with the smooth duration set to 0 for that one
 * call (`Terminal.scrollLines` goes through the viewport, so it would otherwise
 * animate too), before xterm's own handler runs. The buffer is then scrolled
 * back, output no longer moves it, and the animation continues from there.
 */

/** How long one wheel notch takes to glide, in ms. Chosen by the sweep in docs/PERFORMANCE.md. */
export const SMOOTH_SCROLL_MS = 150;

/** The xterm options this module owns. */
export function scrollFeelOptions(): { smoothScrollDuration: number } {
  return { smoothScrollDuration: SMOOTH_SCROLL_MS };
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

/** Installs the guard; the returned function removes it. A terminal that is not open yet has nothing to guard. */
export function attachScrollGuard(terminal: ScrollGuardTerminal): () => void {
  const element = terminal.element;
  if (!element) return () => {};
  const onWheel = (event: WheelEvent) => {
    if (!shouldLeaveBottom(event.deltaY, terminal)) return;
    const duration = terminal.options.smoothScrollDuration;
    terminal.options.smoothScrollDuration = 0;
    try {
      terminal.scrollLines(-1);
    } finally {
      terminal.options.smoothScrollDuration = duration;
    }
  };
  // Capture: before xterm's listener on its scrollable element reads the position.
  element.addEventListener("wheel", onWheel, { capture: true, passive: true });
  return () => element.removeEventListener("wheel", onWheel, { capture: true });
}
