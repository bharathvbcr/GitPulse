/**
 * An interval that stops while the window is hidden.
 *
 * The work-tree status poll already refuses to spend a subprocess on a hidden
 * window (`repos/statusPoll.ts`). The UI's own tickers did not: a five-second
 * timer whose only job is to re-render "just now" kept firing behind other
 * windows, invalidating derived state across a pane every tick, for a label
 * nobody could see. On a laptop that is the difference between a renderer the
 * OS can leave alone and one it cannot.
 *
 * Rejoining on `visibilitychange` runs the callback once immediately, so a
 * window coming back to the front shows current values rather than values from
 * whenever it was hidden — a resumed timer that waits a full period first is
 * how "just now" ends up reading five seconds stale at the moment someone
 * looks at it.
 *
 * Dependency-injected so the scheduling and the visibility source are testable
 * without a DOM.
 *
 * The period is not fixed. Each start asks `decideCadence`, so a timer that
 * is already visibility-aware also slows down when the event loop is late
 * and returns to its base period as that pressure decays. A non-positive
 * period is refused rather than handed to `setInterval` as a spin.
 */
import { addForegroundListener, readBackgroundDocument, removeForegroundListener } from "../runtime/foreground";
import { decideCadence, readEventLoopDelay } from "../runtime/loadCadence";

export interface IntervalHost {
  setInterval(handler: () => void, ms: number): unknown;
  clearInterval(handle: unknown): void;
  addEventListener(type: string, listener: () => void): void;
  removeEventListener(type: string, listener: () => void): void;
  /** True while background work should stop. */
  isHidden(): boolean;
}

/** The real browser host; used when no override is supplied. */
export function browserIntervalHost(): IntervalHost | null {
  if (typeof window === "undefined" || typeof document === "undefined") return null;
  return {
    setInterval: (handler, ms) => window.setInterval(handler, ms),
    clearInterval: (handle) => window.clearInterval(handle as number),
    addEventListener: (type, listener) => addForegroundListener(document, window, type, listener),
    removeEventListener: (type, listener) => removeForegroundListener(type, listener),
    isHidden: () => readBackgroundDocument(),
  };
}

/**
 * Runs `tick` every `ms` while the document is visible.
 *
 * Returns a disposer that removes both the timer and the visibility listener.
 * Safe to call in a context with no DOM: it becomes a no-op rather than
 * throwing, which is what lets components call it unconditionally.
 */
export function createVisibleInterval(
  tick: () => void,
  ms: number,
  host: IntervalHost | null = browserIntervalHost(),
): () => void {
  if (!host) return () => {};

  let handle: unknown = null;
  let period = 0;
  /**
   * Set by the disposer, and checked by everything that could start a timer.
   *
   * The rejoin path below runs `tick()` and then `start()`. A tick is free to
   * dispose this interval — a poll loop that decides it is finished does
   * exactly that — and without this flag `start()` would then create a fresh
   * timer *after* teardown, holding a handle the disposer has already
   * forgotten. Nothing can stop it after that, so it fires until the page
   * goes away: a leak that only appears when a tick disposes from inside
   * itself, which is why it survived the original implementation.
   */
  let disposed = false;

  const stop = () => {
    if (handle === null) return;
    host.clearInterval(handle);
    handle = null;
  };

  const fire = () => {
    if (disposed) return;
    tick();
    // Re-read pressure after the tick. A quiet sample returns the interval
    // to `ms`; a late sample replaces it. The disposed flag is re-checked
    // because the tick is allowed to tear this interval down.
    if (!disposed) start();
  };

  const start = () => {
    if (disposed) return;
    if (host.isHidden()) {
      stop();
      return;
    }
    const decision = decideCadence({
      baseMs: ms,
      lagMs: readEventLoopDelay(),
      paused: false,
    });
    if (!decision.run || decision.delayMs <= 0) {
      stop();
      return;
    }
    if (handle !== null && decision.delayMs === period) return;
    stop();
    period = decision.delayMs;
    handle = host.setInterval(fire, period);
  };

  const onForeground = () => {
    if (disposed) return;
    if (host.isHidden()) {
      stop();
      return;
    }
    // Hide clears the handle, so the show or focus that follows still ticks
    // once. A second foreground event while the interval is already running
    // must not catch up again.
    if (handle !== null) return;
    // Catch up before resuming: whatever the tick renders is stale by however
    // long the window was in the background, and the user is looking at it now.
    tick();
    start();
  };

  if (!host.isHidden()) start();
  host.addEventListener("visibilitychange", onForeground);
  host.addEventListener("focus", onForeground);
  host.addEventListener("blur", onForeground);

  return () => {
    disposed = true;
    stop();
    host.removeEventListener("visibilitychange", onForeground);
    host.removeEventListener("focus", onForeground);
    host.removeEventListener("blur", onForeground);
  };
}
