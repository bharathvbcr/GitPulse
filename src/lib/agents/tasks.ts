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
import type { AttemptNotice, TaskTerminalRequest } from "../terminal/taskLaunches";
import { identityKey, type PathIdentityOptions } from "../repos/paths";
import type { Repository, TaskRun } from "../workbench/client";
import { identityCommonDir, tabMatchesRegistered } from "../workbench/openMembership";
import { currentLocation } from "../workbench/repositoryRelink";
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
  /** A start that failed out of sight (`taskLaunches.ts::attemptNotices`), so the row says so. */
  notices?: (runId: string) => AttemptNotice | undefined;
  /** Terminal slots this window will open. Full when the registry has that many. */
  sessionLimit: number;
  now: number;
  /**
   * The path of a registered repository, by id. A run records only the id
   * and its working directory, and the directory is a checkout, not the
   * repository. Null (or no resolver) leaves the repository unresolved.
   */
  repositoryPath?: (repositoryId: string) => string | null;
}

/**
 * Where each registered repository is, by id.
 *
 * An open tab that is the registered checkout names it, so a task row joins
 * the repository the sweep read under that tab's path. Otherwise the store's
 * own location (its common directory, less `.git`). An identity that carries
 * no local location is left out: the caller reports it as unresolved.
 */
export function repositoryPaths(
  repositories: readonly Pick<Repository, "id" | "identity_key">[],
  tabs: readonly { path: string }[],
  paths: PathIdentityOptions,
): Map<string, string> {
  const found = new Map<string, string>();
  for (const repository of repositories) {
    const tab = tabs.find((item) => tabMatchesRegistered(item.path, repository.identity_key, paths));
    if (tab) {
      found.set(repository.id, tab.path);
      continue;
    }
    if (!identityCommonDir(repository.identity_key)) continue;
    const location = currentLocation(repository);
    if (identityKey(location, paths)) found.set(repository.id, location);
  }
  return found;
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
    notices: watch.notices,
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
      repoPath: watch.repositoryPath?.(run.repository_id) ?? "",
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
