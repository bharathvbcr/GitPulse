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
 */
export interface IntervalHost {
  setInterval(handler: () => void, ms: number): unknown;
  clearInterval(handle: unknown): void;
  addEventListener(type: string, listener: () => void): void;
  removeEventListener(type: string, listener: () => void): void;
  /** True while the document is hidden. */
  isHidden(): boolean;
}

/** The real browser host; used when no override is supplied. */
export function browserIntervalHost(): IntervalHost | null {
  if (typeof window === "undefined" || typeof document === "undefined") return null;
  return {
    setInterval: (handler, ms) => window.setInterval(handler, ms),
    clearInterval: (handle) => window.clearInterval(handle as number),
    addEventListener: (type, listener) => document.addEventListener(type, listener),
    removeEventListener: (type, listener) => document.removeEventListener(type, listener),
    isHidden: () => document.hidden,
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

  const stop = () => {
    if (handle === null) return;
    host.clearInterval(handle);
    handle = null;
  };

  const start = () => {
    if (handle !== null) return;
    handle = host.setInterval(tick, ms);
  };

  const onVisibilityChange = () => {
    if (host.isHidden()) {
      stop();
      return;
    }
    // Catch up before resuming: whatever the tick renders is stale by however
    // long the window was hidden, and the user is looking at it now.
    tick();
    start();
  };

  if (!host.isHidden()) start();
  host.addEventListener("visibilitychange", onVisibilityChange);

  return () => {
    stop();
    host.removeEventListener("visibilitychange", onVisibilityChange);
  };
}
