/**
 * A timeout loop whose period follows `decideCadence`.
 *
 * `setInterval` cannot change its delay, so a timer that must slow down when
 * the event loop is late and stop entirely while the window is hidden has to
 * re-arm itself. The re-arm is skipped after dispose, including when the tick
 * disposes the timer — the same leak `createVisibleInterval` already closes.
 */

import { addForegroundListener, readBackgroundDocument, removeForegroundListener } from "./foreground";
import { decideCadence, readEventLoopDelay } from "./loadCadence";

export interface AdaptiveHost {
  setTimeout(handler: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
  addEventListener(type: string, listener: () => void): void;
  removeEventListener(type: string, listener: () => void): void;
  /** True while background work should stop. */
  isHidden(): boolean;
}

/** The real browser host. A DOM-free caller gets null and the timer no-ops. */
export function browserAdaptiveHost(): AdaptiveHost | null {
  if (typeof window === "undefined" || typeof document === "undefined") return null;
  return {
    setTimeout: (handler, ms) => window.setTimeout(handler, ms),
    clearTimeout: (handle) => window.clearTimeout(handle as number),
    addEventListener: (type, listener) => addForegroundListener(document, window, type, listener),
    removeEventListener: (type, listener) => removeForegroundListener(type, listener),
    isHidden: () => readBackgroundDocument(),
  };
}

export interface AdaptiveTimerOptions {
  maxMs?: number;
  /** Defaults to the process-wide sample the responsiveness probe writes. */
  readLag?: () => number;
}

/**
 * Runs `tick` on a cadence that stretches under load and stops in the background.
 *
 * The first arm waits one period, matching `setInterval`. Coming back from
 * the background runs `tick` once immediately, then arms again: a label or a
 * poll that waits a full period after the window returns is already stale.
 * A second foreground event while that arm is already waiting does not tick
 * again.
 */
export function createAdaptiveTimer(
  tick: () => void,
  baseMs: number,
  host: AdaptiveHost | null = browserAdaptiveHost(),
  options: AdaptiveTimerOptions = {},
): () => void {
  if (!host) return () => {};
  const readLag = options.readLag ?? readEventLoopDelay;
  let handle: unknown = null;
  let disposed = false;

  const clear = () => {
    if (handle === null) return;
    host.clearTimeout(handle);
    handle = null;
  };

  const arm = () => {
    if (disposed || handle !== null || host.isHidden()) return;
    const decision = decideCadence({
      baseMs,
      maxMs: options.maxMs,
      lagMs: readLag(),
      paused: false,
    });
    // A refused decision must not become a zero-delay timeout. Those spin.
    if (!decision.run || !Number.isFinite(decision.delayMs) || decision.delayMs <= 0) return;
    handle = host.setTimeout(() => {
      handle = null;
      if (disposed || host.isHidden()) return;
      try {
        tick();
      } finally {
        if (!disposed) arm();
      }
    }, decision.delayMs);
  };

  const onForeground = () => {
    if (disposed) return;
    if (host.isHidden()) {
      clear();
      return;
    }
    // Hide clears the handle, so the show or focus that follows still ticks
    // once. A focus event that arrives while the timer is already armed must
    // not catch up a second time.
    if (handle !== null) return;
    try {
      tick();
    } finally {
      if (!disposed) arm();
    }
  };

  if (!host.isHidden()) arm();
  host.addEventListener("visibilitychange", onForeground);
  host.addEventListener("focus", onForeground);
  host.addEventListener("blur", onForeground);
  return () => {
    disposed = true;
    clear();
    host.removeEventListener("visibilitychange", onForeground);
    host.removeEventListener("focus", onForeground);
    host.removeEventListener("blur", onForeground);
  };
}
