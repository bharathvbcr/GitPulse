import { decideCadence } from "../runtime/loadCadence";

/** Base period for a task run that is still starting, running, or prepared. */
export const TASK_RUN_POLL_MS = 1_500;

export interface TaskRunPollInput {
  baseMs: number;
  lagMs: number;
  /** Window hidden, or the webview has lost focus. */
  background: boolean;
  /** A run is starting, running, or still inside its prepared window. */
  live: boolean;
  /** The agent pane is the one on screen. */
  active: boolean;
}

/**
 * Delay until the next live-run poll, or null when the loop must stay stopped.
 *
 * Each flag has to be the boolean the caller means. A missing or non-boolean
 * flag refuses to poll rather than treating "unknown" as "running".
 */
export function nextTaskRunPollDelay(input: TaskRunPollInput): number | null {
  if (input.active !== true || input.background !== false || input.live !== true) return null;
  const decision = decideCadence({
    baseMs: input.baseMs,
    lagMs: input.lagMs,
    paused: false,
  });
  if (!decision.run || !Number.isFinite(decision.delayMs) || decision.delayMs <= 0) return null;
  return decision.delayMs;
}
