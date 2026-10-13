/**
 * The one bound on how long the frontend waits for an answer.
 *
 * Every ad-hoc "race the call against a timer" helper used to be its own copy
 * (Overview, task actions, task terminal, task delete, devmap diagnostics),
 * and the IPC path itself had none: a backend call that never answered left
 * its spinner up for good. `withDeadline` is the single implementation, and
 * `ipc/invoke.ts` applies it to every call that carries a deadline.
 *
 * It stops WAITING, not the native operation: a late answer or rejection is
 * consumed so it cannot surface as an unhandled rejection, and the caller has
 * already been told the wait ended. The caller decides what a timeout means
 * for a write (see `outcomeUnknown` in deferral.ts).
 */
export class DeadlineError extends Error {
  readonly ms: number;
  constructor(message: string, ms: number) {
    super(message);
    this.name = "DeadlineError";
    this.ms = ms;
  }
}

export function isDeadlineError(error: unknown): error is DeadlineError {
  return error instanceof DeadlineError;
}

export type DeadlineOptions = {
  /** Ends the wait at once with the signal's reason, and never starts `work` if already aborted. */
  signal?: AbortSignal;
  /** The timeout error's message; the default names the wait. */
  message?: string;
  /** Builds the error to reject with on timeout, for callers with their own error type. */
  timeoutError?: (message: string, ms: number) => Error;
};

function abortReason(signal: AbortSignal): unknown {
  return signal.reason ?? new DOMException("The operation was aborted.", "AbortError");
}

/**
 * `work`'s answer, or a rejection once `ms` pass or `signal` aborts. A
 * function is started only if the wait is still open, so an expired budget or
 * an already-aborted signal never sends the call.
 */
export function withDeadline<T>(
  work: Promise<T> | (() => Promise<T>),
  ms: number,
  options: DeadlineOptions = {},
): Promise<T> {
  const { signal } = options;
  const message = options.message ?? `Timed out after ${Math.round(ms)} ms`;
  const timeoutError = options.timeoutError ?? ((text, limit) => new DeadlineError(text, limit));
  if (signal?.aborted) return Promise.reject(abortReason(signal));
  if (!(ms > 0)) return Promise.reject(timeoutError(message, ms));
  return new Promise<T>((resolve, reject) => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onAbort = () => finish(() => reject(abortReason(signal as AbortSignal)));
    function finish(settle: () => void): void {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      settle();
    }
    // `Infinity` waits for the signal alone; `setTimeout` would fire it at once.
    if (Number.isFinite(ms)) timer = setTimeout(() => finish(() => reject(timeoutError(message, ms))), ms);
    signal?.addEventListener("abort", onAbort, { once: true });
    // `finish` is idempotent in effect: a settled promise ignores later
    // resolve/reject calls, and the late rejection is consumed here.
    Promise.resolve()
      .then(typeof work === "function" ? work : () => work)
      .then((value) => finish(() => resolve(value)), (error: unknown) => finish(() => reject(error)));
  });
}

/**
 * For `.catch` on a `withDeadline`: a deadline becomes `fallback`, anything
 * else (the work's own rejection, an abort) still propagates.
 */
export function fallbackOnDeadline<T>(fallback: T): (error: unknown) => T {
  return (error) => {
    if (isDeadlineError(error)) return fallback;
    throw error;
  };
}
