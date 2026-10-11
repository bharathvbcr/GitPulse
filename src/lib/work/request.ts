/** Overview has one overall load deadline, including every secondary probe. */
export const WORK_TIMEOUT_MS = 20_000;

export function workTimeout(value = WORK_TIMEOUT_MS): number {
  return Number.isFinite(value) && value > 0 ? Math.min(WORK_TIMEOUT_MS, Math.max(1, value)) : WORK_TIMEOUT_MS;
}

/** The timeout error's text; the wait itself is `withDeadline` (../async/deadline.ts). */
export const WORK_DEADLINE_MESSAGE = "Overview refresh deadline exceeded";
