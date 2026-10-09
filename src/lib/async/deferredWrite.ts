/**
 * A storage write that runs at most once per window, plus a flush for when
 * the page is going away.
 *
 * Not a trailing debounce. A debounce restarts its timer on every call, so a
 * steady stream — an error storm, a pointer drag — never goes quiet and the
 * write waits for the stream to end. Here the first request in a window arms
 * the timer and later requests ride it, so the write lags the newest state by
 * at most `delayMs` however busy the caller is. The write reads live state
 * when it runs, which is why `schedule` takes no arguments.
 */

export interface DeferredWrite {
  /** Requests a write. Requests made while one is pending share it. */
  schedule(): void;
  /** Runs the pending write now. Does nothing when none is pending. */
  flush(): void;
  /** Drops the pending write. */
  cancel(): void;
  /** Whether a write is waiting for its timer. */
  readonly pending: boolean;
}

export interface DeferredWriteTimers {
  setTimeout(handler: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

const globalTimers: DeferredWriteTimers = {
  setTimeout: (handler, ms) => setTimeout(handler, ms),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export function createDeferredWrite(
  write: () => void,
  delayMs: number,
  timers: DeferredWriteTimers = globalTimers,
): DeferredWrite {
  if (!Number.isFinite(delayMs) || delayMs <= 0) {
    throw new RangeError(`createDeferredWrite: delayMs must be a positive number, got ${delayMs}`);
  }
  let handle: unknown = null;

  const clear = () => {
    if (handle === null) return;
    timers.clearTimeout(handle);
    handle = null;
  };

  return {
    schedule() {
      if (handle !== null) return;
      handle = timers.setTimeout(() => {
        handle = null;
        write();
      }, delayMs);
    },
    flush() {
      if (handle === null) return;
      clear();
      write();
    },
    cancel: clear,
    get pending() {
      return handle !== null;
    },
  };
}

interface LifecycleTarget {
  addEventListener(type: string, listener: () => void): void;
  removeEventListener(type: string, listener: () => void): void;
}

interface LifecycleDocument extends LifecycleTarget {
  readonly visibilityState?: string;
}

function lifecycleTarget<T extends LifecycleTarget>(value: unknown): T | null {
  return typeof value === "object" &&
    value !== null &&
    typeof (value as Partial<LifecycleTarget>).addEventListener === "function" &&
    typeof (value as Partial<LifecycleTarget>).removeEventListener === "function"
    ? (value as T)
    : null;
}

/**
 * Runs `flush` when the page is hidden or unloaded.
 *
 * `pagehide` is the unload signal; a hidden document is the last moment a
 * WebKit page is guaranteed to run script before it may be suspended or
 * killed without one. Either may be absent — tests stub `window` as a bare
 * `{ localStorage }` — and then that half is simply not wired. Returns the
 * unbinder.
 */
export function flushOnPageHide(
  flush: () => void,
  win: unknown = typeof window === "undefined" ? null : window,
  doc: unknown = typeof document === "undefined" ? null : document,
): () => void {
  const pageTarget = lifecycleTarget(win);
  const visibilityTarget = lifecycleTarget<LifecycleDocument>(doc);
  const onVisibility = () => {
    if (visibilityTarget?.visibilityState === "hidden") flush();
  };
  pageTarget?.addEventListener("pagehide", flush);
  visibilityTarget?.addEventListener("visibilitychange", onVisibility);
  return () => {
    pageTarget?.removeEventListener("pagehide", flush);
    visibilityTarget?.removeEventListener("visibilitychange", onVisibility);
  };
}
