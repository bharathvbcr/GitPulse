/**
 * The backend's spawn gate declines to start more git under load and says so
 * with one marker (`DEFERRED_MARKER` in src-tauri/src/engine/git_cli.rs). A
 * deferral is not a failure of the repository or of git: nothing ran, and the
 * same read may succeed a few seconds later. It must not be reported as an
 * error, and it must not be retried at once, straight back into the limit that
 * refused it.
 *
 * The marker is checked against the Rust source by deferral.test.ts, so the
 * two cannot drift apart silently.
 */
export const DEFERRED_UNDER_LOAD_MARKER = " deferred under load after ";

export function isDeferredUnderLoad(message: string): boolean {
  return message.includes(DEFERRED_UNDER_LOAD_MARKER);
}

/**
 * Delay before asking again after the `attempt`-th consecutive deferral
 * (1-based): 3 s, 6 s, 12 s, 24 s, then 30 s. The first is longer than the
 * gate's 2 s queue budget, so a retry never lands inside the window that just
 * refused it.
 */
export function deferredRetryDelayMs(attempt: number): number {
  const step = Math.max(1, Math.floor(attempt));
  return Math.min(3_000 * 2 ** (step - 1), 30_000);
}

/** Consecutive deferrals after which the deferral is shown as the error. */
export const MAX_DEFERRED_RETRIES = 5;
