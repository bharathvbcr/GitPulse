/**
 * Reading one agent run's ledger rows, for its activity timeline.
 *
 * `runTimeline.ts` is the pure projection; this is the part that knows which
 * session ids a run owns and asks the backend for their rows.
 */
import { invoke } from "../ipc/invoke";
import type { TaskRun } from "../workbench/client";
import type { RunWindow } from "./runTimeline";
import type { LedgerEvent } from "./types";

/**
 * Rows read for one run, newest first by the backend and returned oldest
 * first. A run that left more than this keeps its newest; the timeline says it
 * was capped (`RunTimeline.capped`) rather than presenting the window as the
 * whole run.
 */
export const RUN_ACTIVITY_ROWS = 400;

/**
 * The ids a run's rows are filed under: the terminal's (`session_id`, which the
 * spawn and lifecycle rows carry) and the agent's own (`id`, which the
 * transcript rows carry). The run record is the bridge between them, so no row
 * is ever rewritten to join the two.
 */
export function runSessionIds(run: Pick<TaskRun, "id" | "session_id">): string[] {
  const ids = [run.id, run.session_id].filter((id): id is string => typeof id === "string" && id !== "");
  return [...new Set(ids)];
}

/** Whether an attempt in this state is still going. */
const LIVE_STATES: readonly TaskRun["state"][] = ["starting", "running"];

/** The run's own clock, for the segments the lifecycle rows cannot bound. */
export function runWindowOf(run: Pick<TaskRun, "state" | "claimed_at" | "started_at" | "finished_at">): RunWindow {
  return {
    claimedAt: run.claimed_at ?? null,
    startedAt: run.started_at ?? null,
    finishedAt: run.finished_at ?? null,
    live: LIVE_STATES.includes(run.state),
  };
}

/** Reads the rows of `run`, found from its checkout's repository. */
export function readRunRows(run: Pick<TaskRun, "id" | "session_id" | "cwd">): Promise<LedgerEvent[]> {
  return invoke<LedgerEvent[]>("cmd_ledger_tail", {
    repoPath: run.cwd,
    cursor: 0,
    limit: RUN_ACTIVITY_ROWS,
    newest: true,
    sessionIds: runSessionIds(run),
    action: null,
  });
}
