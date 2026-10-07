/**
 * Task attempts, reduced to the facts the Agents plane can say out loud.
 *
 * The judgement stays in `monitorAttempt`. This file only records whether
 * that judgement was available: a board that has not been read, a board
 * whose read failed, and a board whose run list was capped are three
 * different reports. An empty run list after a successful read is none.
 */

import type { SessionActivity } from "../terminal/sessionActivity";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";
import type { TaskTerminalRequest } from "../terminal/taskLaunches";
import type { TaskRun } from "../workbench/client";
import { monitorAttempt, type MonitorContext, type PendingRequests } from "../workbench/taskSessions";
import type { PlaneTask, TaskProbe } from "./plane";

export interface TaskBoardRead {
  runs: readonly TaskRun[];
  complete: boolean;
  pending: ReadonlyMap<string, PendingRequests>;
  /** Null before the first read. Not the same as a read that failed. */
  readAt: number | null;
  error: string | null;
}

export interface TaskBoardWatch {
  records: readonly TerminalSessionRecord[];
  requests: readonly TaskTerminalRequest[];
  activity: (sessionId: string) => SessionActivity | undefined;
  /** Terminal slots this window will open. Full when the registry has that many. */
  sessionLimit: number;
  now: number;
}

export function taskProbeFromBoard(board: TaskBoardRead, watch: TaskBoardWatch): TaskProbe {
  if (board.readAt === null && !board.error) {
    return { ok: false, unread: true, error: "", complete: false, tasks: [] };
  }
  if (board.error) {
    return { ok: false, unread: false, error: board.error, complete: false, tasks: [] };
  }
  const clock = board.readAt ?? watch.now;
  const context: MonitorContext = {
    records: watch.records,
    requests: watch.requests,
    capacityFull: watch.sessionLimit > 0 && watch.records.length >= watch.sessionLimit,
    activity: watch.activity,
    pending: (runId) => board.pending.get(runId),
    now: watch.now,
    clock,
  };
  const tasks: PlaneTask[] = board.runs.map((run) => {
    const monitored = monitorAttempt(run, context);
    const pending = board.pending.get(run.id);
    return {
      runId: run.id,
      title: run.task_title || run.task_id || run.id,
      repoPath: run.cwd,
      cwd: run.cwd,
      provider: run.provider,
      tone: monitored.tone,
      disconnected: monitored.disconnected,
      unstarted: monitored.unstarted,
      pendingCount: pending?.count ?? 0,
      pendingMore: pending?.more ?? false,
    };
  });
  return { ok: true, unread: false, error: "", complete: board.complete, tasks };
}
