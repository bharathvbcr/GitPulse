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
import { writable, type Readable } from "svelte/store";
import type { PathIdentityOptions } from "../repos/paths";
import { listRepositories, type Page, type Repository, type TaskRun } from "../workbench/client";
import type { OpenTabRef, RegisteredRef } from "../workbench/openMembership";
import { checkoutCandidates, preferredCheckout } from "../workbench/taskHandoff";
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
  /**
   * The checkout a registered repository is known by, or null when it is not
   * known. A run records only `repository_id` and its own `cwd`, and the cwd
   * is where the agent works, not which repository it works for.
   */
  repositoryPath: (repositoryId: string) => string | null;
}

/**
 * Registered repositories by id, each at the checkout the plane knows it by.
 *
 * An open tab wins, so a task groups with the checkouts swept from that tab.
 * A closed repository falls back to the folder beside its common directory,
 * the same rule the task handoff uses. An identity with neither is left out.
 */
export function repositoryPaths(
  repositories: readonly RegisteredRef[],
  openTabs: readonly OpenTabRef[],
  paths: PathIdentityOptions,
): Map<string, string> {
  const found = new Map<string, string>();
  for (const repository of repositories) {
    const path = preferredCheckout(checkoutCandidates(repository.id, repositories, openTabs, paths));
    if (path) found.set(repository.id, path);
  }
  return found;
}

/** Pages of registered repositories one read will follow. */
export const MAX_REPOSITORY_PAGES = 10;

/**
 * Every registered repository, up to the page cap. `complete` is false when
 * the cap stopped the read; an id past it stays unresolved rather than guessed.
 */
export async function readRegisteredRepositories(
  list: (cursor?: string) => Promise<Page<Repository>> = listRepositories,
): Promise<{ repositories: Repository[]; complete: boolean }> {
  const repositories: Repository[] = [];
  let cursor: string | undefined;
  for (let page = 0; page < MAX_REPOSITORY_PAGES; page += 1) {
    const next = await list(cursor);
    repositories.push(...next.items);
    if (!next.has_more || !next.next_cursor) return { repositories, complete: true };
    cursor = next.next_cursor;
  }
  return { repositories, complete: false };
}

export interface RegisteredRepositories extends Readable<readonly Repository[]> {
  /** Reads the registry when one of these ids is not among those already read. */
  want(repositoryIds: readonly string[]): void;
}

/**
 * The registered repositories the plane names task attempts by.
 *
 * Read once, then again only when a run names an id the last read did not
 * have. One read at a time, and once per set of unknown ids: a deleted
 * repository stays unknown, and asking again for it after every answer would
 * never stop. A failed read leaves the last answer in place.
 */
export function createRegisteredRepositories(
  read: () => Promise<{ repositories: Repository[] }> = () => readRegisteredRepositories(),
): RegisteredRepositories {
  const store = writable<readonly Repository[]>([]);
  let known = new Set<string>();
  let tried: string | null = null;
  let reading = false;

  function want(repositoryIds: readonly string[]): void {
    if (reading) return;
    const unknown = [...new Set(repositoryIds.filter((id) => id && !known.has(id)))].sort().join("\n");
    if (tried !== null && (unknown === "" || unknown === tried)) return;
    tried = unknown;
    reading = true;
    void read()
      .then(
        (found) => {
          known = new Set(found.repositories.map((repository) => repository.id));
          store.set(found.repositories);
        },
        () => {
          // Unread stays unread: task rows then say the repository was not resolved.
        },
      )
      .finally(() => {
        reading = false;
      });
  }

  return { subscribe: store.subscribe, want };
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
      repoPath: (run.repository_id && watch.repositoryPath(run.repository_id)) || "",
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
