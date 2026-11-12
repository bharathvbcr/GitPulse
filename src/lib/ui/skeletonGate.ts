/**
 * When a loading placeholder may be seen.
 *
 * A load that finishes inside the Doherty band should never flash a skeleton,
 * and a skeleton that did appear should not vanish the instant the data lands:
 * either reads as a flicker. So a placeholder stays invisible for
 * `SKELETON_SHOW_DELAY_MS` and, once visible, holds for at least
 * `SKELETON_MIN_HOLD_MS` before it may leave (DESIGN_LANGUAGE §8.4).
 *
 * The timing is policy, not motion: it is not scaled by reduced motion.
 */
export const SKELETON_SHOW_DELAY_MS = 300;
export const SKELETON_MIN_HOLD_MS = 400;

/** The clock and timer the gate reads, replaceable so a test owns time. */
export interface GateClock {
  now(): number;
  setTimeout(callback: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

const realClock: GateClock = {
  now: () => Date.now(),
  setTimeout: (callback, ms) => globalThis.setTimeout(callback, ms),
  clearTimeout: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export interface SkeletonGate {
  /** Stops a pending show. A skeleton already shown keeps its hold clock. */
  cancel(): void;
  /** Milliseconds a visible skeleton must still stay; 0 if never shown or held long enough. */
  holdRemainingMs(): number;
}

/**
 * Starts the show delay. `onShow` fires once, after the delay, unless
 * `cancel()` came first.
 */
export function startSkeletonGate(
  onShow: () => void,
  clock: GateClock = realClock,
  showDelayMs: number = SKELETON_SHOW_DELAY_MS,
  minHoldMs: number = SKELETON_MIN_HOLD_MS,
): SkeletonGate {
  let shownAt: number | null = null;
  let cancelled = false;
  const handle = clock.setTimeout(() => {
    if (cancelled) return;
    shownAt = clock.now();
    onShow();
  }, showDelayMs);
  return {
    cancel() {
      cancelled = true;
      clock.clearTimeout(handle);
    },
    holdRemainingMs() {
      if (shownAt === null) return 0;
      return Math.max(0, minHoldMs - (clock.now() - shownAt));
    },
  };
}
