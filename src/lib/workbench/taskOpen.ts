/**
 * Opening a task from outside the task board — the back link from a terminal
 * session to the task it is working on.
 *
 * The board is mounted lazily the first time the Tasks surface is shown, so a
 * direct call into it would race its mount and land nowhere. The request is
 * queued instead, then the surface is switched; the global board consumes it
 * once it is active and idle (see `TaskBoard.svelte`, `acceptsOpenRequests`).
 * One pending id, not a counter: a counter aims at whichever board instance
 * happens to read it, and a repeated request for the same task must still
 * open it after the first was consumed.
 *
 * Only the global board consumes. A repository's own board is filtered to that
 * repository and may not show the task at all.
 */

import { get, writable } from "svelte/store";
import { interfaceStore } from "../stores/interfaceStore";
import { getTaskRun, type TaskRun } from "./client";

const pending = writable<string | null>(null);
export const taskOpenRequest = { subscribe: pending.subscribe };

/** The store's id grammar (dc-store `Input::id`), checked before it is queued. */
export function isTaskId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
}

/** Queues `taskId` and shows the Tasks surface, where the board opens it. */
export function requestTaskOpen(taskId: string): void {
  if (!isTaskId(taskId)) throw new Error("That task cannot be opened: its id is not valid.");
  pending.set(taskId);
  interfaceStore.setGlobalSurface("tasks");
}

/** Takes the pending request, if it is still `taskId`. Returns whether it was. */
export function consumeTaskOpen(taskId: string): boolean {
  if (get(pending) !== taskId) return false;
  pending.set(null);
  return true;
}

/**
 * Opens the task a run belongs to. The run is read rather than trusted from
 * the caller: a session knows its run id (an adopted one knows nothing else),
 * and the run record is the one place that names its task.
 */
export async function openTaskForRun(
  runId: string,
  read: (id: string) => Promise<Pick<TaskRun, "id" | "task_id">> = getTaskRun,
): Promise<void> {
  const run = await read(runId);
  if (run.id !== runId) throw new Error("That attempt's record could not be read.");
  requestTaskOpen(run.task_id);
}
