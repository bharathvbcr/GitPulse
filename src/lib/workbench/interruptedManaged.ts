/**
 * Managed (Manvi) agents a crash or reboot killed mid-work.
 *
 * A managed run has no terminal tab, so the session ledger never offers it.
 * After a reboot its run is released as `exited` with `outcome_uncertain` and
 * no exit code: nobody saw it end, and it was not finished. The app-level
 * notice counts these separately from the terminal agents and links to the
 * task, because the recovery is "review what it changed", not "reopen".
 */

import { get, writable, type Readable } from "svelte/store";
import { formatError } from "../ui/formatError";
import { listRecentExitedRuns, type TaskRun } from "./client";
import { requestTaskOpen } from "./taskOpen";

/**
 * How long after release an uncertain run still counts as interrupted. The
 * reconciler stamps `finished_at` when it notices, i.e. shortly after the
 * reboot, so this is "since the last launch" without a launch clock; older
 * ones stay in the task's own history and stop nagging.
 */
export const INTERRUPTED_MANAGED_WINDOW_SECONDS = 6 * 3600;

/** Managed runs released with an unobserved outcome, within the window, newest first. */
export function recentUncertainManagedRuns(
  runs: readonly TaskRun[],
  nowSeconds: number,
  windowSeconds: number = INTERRUPTED_MANAGED_WINDOW_SECONDS,
): TaskRun[] {
  return runs
    .filter((run) => run.kind === "managed" && run.state === "exited" && run.outcome_uncertain)
    .filter((run) => (run.finished_at ?? run.updated_at) >= nowSeconds - windowSeconds)
    .sort((a, b) => (b.finished_at ?? b.updated_at) - (a.finished_at ?? a.updated_at));
}

export type InterruptedManaged =
  | { status: "unread"; runs: TaskRun[] }
  | { status: "ok"; runs: TaskRun[]; complete: boolean }
  | { status: "failed"; runs: TaskRun[]; error: string };

const store = writable<InterruptedManaged>({ status: "unread", runs: [] });
export const interruptedManagedStore: Readable<InterruptedManaged> = { subscribe: store.subscribe };

/** Replaces the snapshot in unit tests. */
export function setInterruptedManagedForTest(value: InterruptedManaged): void {
  store.set(value);
}

/** Reads the recent uncertain managed runs. A failed read is recorded as failed, never as "none". */
export async function refreshInterruptedManaged(
  read: typeof listRecentExitedRuns = listRecentExitedRuns,
  nowSeconds: number = Date.now() / 1000,
): Promise<void> {
  try {
    const { runs, complete } = await read();
    store.set({ status: "ok", runs: recentUncertainManagedRuns(runs, nowSeconds), complete });
  } catch (cause) {
    store.set({ status: "failed", runs: get(store).runs, error: formatError(cause) });
  }
}

/** "2 managed agents were interrupted — open their tasks" */
export function interruptedManagedText(count: number): string {
  return count === 1
    ? "1 managed agent was interrupted — open its task."
    : `${count} managed agents were interrupted — open their tasks.`;
}

/** Opens the task of the newest interrupted run: the board shows it, and the rest are a click away there. */
export function openInterruptedManaged(runs: readonly TaskRun[], open: (taskId: string) => void = requestTaskOpen): void {
  const first = runs[0];
  if (first) open(first.task_id);
}
