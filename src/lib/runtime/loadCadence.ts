/**
 * How often background work may run, given what the machine is doing now.
 *
 * The responsiveness probe measures event-loop delay and used to only log it.
 * Polls and label clocks kept a fixed period, and several of them still woke
 * while the window was hidden — the callback returned, the timer did not.
 * This is the decision those timers share: pause when nobody is looking,
 * stretch when the loop is already late, and refuse an interval that would spin.
 *
 * Pure aside from the single latest delay sample. Callers own their timers.
 */

/** Delay at or above this stretches the next period. Matches the UI probe. */
export const LAG_STRETCH_MS = 250;
/** Delay at or above this stretches harder. Still not a sleep-sized gap. */
export const LAG_HARD_MS = 750;
/**
 * A gap this large is sleep or a suspended web view, not a busy loop.
 * Treating it as pressure would keep every poll slow for minutes after wake.
 */
export const SUSPEND_GAP_MS = 30_000;
/** No adaptive delay is allowed to exceed this, whatever the caller asked. */
export const CADENCE_CEILING_MS = 120_000;

export interface CadenceInput {
  /** Requested period while the machine is quiet. */
  baseMs: number;
  /** Upper bound. Omitted, non-finite, or below `baseMs` uses the default. */
  maxMs?: number;
  /**
   * Latest event-loop delay in milliseconds.
   * Non-finite or negative is pressure, not health: a broken reading must
   * not look like a quiet machine.
   */
  lagMs: number;
  /** Hidden window, or any other reason the caller must not run. */
  paused: boolean;
}

export interface CadenceDecision {
  /** False means do not run and do not arm a timer. */
  run: boolean;
  /** Milliseconds until the next arm. Zero when `run` is false. Always finite. */
  delayMs: number;
  /** Always set. An empty reason is indistinguishable from a missing decision. */
  reason: string;
}

function defaultMax(base: number): number {
  const scaled = base * 8;
  if (!Number.isFinite(scaled) || scaled <= base) return Math.min(CADENCE_CEILING_MS, base);
  return Math.min(CADENCE_CEILING_MS, scaled);
}

function resolveMax(base: number, maxMs: number | undefined): number {
  if (maxMs === undefined || !Number.isFinite(maxMs) || maxMs < base) return defaultMax(base);
  return Math.min(CADENCE_CEILING_MS, maxMs);
}

/**
 * Decides the next delay. Never returns a non-finite delay, and never returns
 * a positive delay when `run` is false — that pair is what becomes
 * `setInterval(fn, 0)`.
 */
export function decideCadence(input: CadenceInput): CadenceDecision {
  const base = input.baseMs;
  if (!Number.isFinite(base) || base <= 0) {
    return { run: false, delayMs: 0, reason: "refusing a non-positive interval" };
  }
  // Only an explicit false is "running". Undefined would otherwise pass the
  // `if (paused)` check and schedule work the caller forgot to classify.
  if (input.paused !== false) {
    return {
      run: false,
      delayMs: 0,
      reason: input.paused === true ? "paused" : "pause flag is not a boolean",
    };
  }

  const max = resolveMax(base, input.maxMs);
  let lag = input.lagMs;
  let reason = "quiet";
  if (!Number.isFinite(lag) || lag < 0) {
    lag = LAG_HARD_MS;
    reason = "non-finite lag treated as pressure";
  } else if (lag >= SUSPEND_GAP_MS) {
    return { run: true, delayMs: base, reason: "suspend gap is not load" };
  } else if (lag >= LAG_HARD_MS) {
    reason = "event loop hard-late";
  } else if (lag >= LAG_STRETCH_MS) {
    reason = "event loop late";
  }

  const factor = lag >= LAG_HARD_MS ? 4 : lag >= LAG_STRETCH_MS ? 2 : 1;
  const scaled = base * factor;
  const delayMs = Math.min(max, Number.isFinite(scaled) ? Math.max(base, scaled) : max);
  if (!Number.isFinite(delayMs) || delayMs <= 0) {
    return { run: false, delayMs: 0, reason: "refusing a non-positive delay" };
  }
  return { run: true, delayMs, reason };
}

/** Latest observed delay. One number, not a log: a history here would grow for the life of the window. */
let sample = 0;

/**
 * Records one event-loop delay.
 *
 * Pressure rises on the sample that observed it. It decays halfway per later
 * sample, so one healthy tick does not undo a stall and one stall does not
 * stick until restart. A suspend-sized gap clears pressure: the machine was
 * asleep, not busy.
 */
export function noteEventLoopDelay(ms: number): void {
  if (!Number.isFinite(ms) || ms < 0) {
    sample = LAG_HARD_MS;
    return;
  }
  if (ms >= SUSPEND_GAP_MS) {
    sample = 0;
    return;
  }
  sample = ms > sample ? ms : sample * 0.5 + ms * 0.5;
}

export function readEventLoopDelay(): number {
  return sample;
}

export function resetEventLoopDelay(): void {
  sample = 0;
}
