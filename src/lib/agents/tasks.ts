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
import { writable, type Readable } from "svelte/store";
import { identityKey, type PathIdentityOptions } from "../repos/paths";
import { explainError, listRepositories, type Page, type Repository, type TaskRun } from "../workbench/client";
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

/** How long a failed registry read waits before a run may ask again. */
export const REPOSITORY_RETRY_MS = 30_000;

/** What the last registry read established, beside the repositories it found. */
export interface RegisteredRepositoriesStatus {
  /** False when the page cap stopped the last successful read. */
  complete: boolean;
  /** Why the last read failed; empty when it succeeded or none has run. */
  error: string;
}

export interface RegisteredRepositories extends Readable<readonly Repository[]> {
  /** Whether the list is capped, and why the last read failed, if it did. */
  status: Readable<RegisteredRepositoriesStatus>;
  /** Reads the registry when one of these ids is not among those already read. */
  want(repositoryIds: readonly string[]): void;
  /** Reads again now, whatever the last read found. */
  refresh(): void;
}

/**
 * The registered repositories the plane names task attempts by.
 *
 * Read once, then again only when a run names an id the last read did not
 * have. One read at a time, and once per set of unknown ids: a deleted
 * repository stays unknown, and asking again for it after every answer would
 * never stop. A failed read leaves the last answer in place, says why in
 * `status`, and is retried on the next ask after `REPOSITORY_RETRY_MS`, or at
 * once by `refresh`.
 */
export function createRegisteredRepositories(
  read: () => Promise<{ repositories: Repository[]; complete?: boolean }> = () => readRegisteredRepositories(),
  now: () => number = () => Date.now(),
): RegisteredRepositories {
  const store = writable<readonly Repository[]>([]);
  const status = writable<RegisteredRepositoriesStatus>({ complete: true, error: "" });
  let known = new Set<string>();
  let tried: string | null = null;
  let retryAt: number | null = null;
  let reading = false;
  let lastIds: readonly string[] = [];

  function start(unknown: string): void {
    tried = unknown;
    reading = true;
    void read()
      .then(
        (found) => {
          known = new Set(found.repositories.map((repository) => repository.id));
          retryAt = null;
          store.set(found.repositories);
          status.set({ complete: found.complete !== false, error: "" });
        },
        (cause: unknown) => {
          // Unread stays unread until the retry: task rows say the repository was not resolved.
          retryAt = now() + REPOSITORY_RETRY_MS;
          status.update((last) => ({ ...last, error: explainError(cause) }));
        },
      )
      .finally(() => {
        reading = false;
      });
  }

  function unknownOf(repositoryIds: readonly string[]): string {
    return [...new Set(repositoryIds.filter((id) => id && !known.has(id)))].sort().join("\n");
  }

  function want(repositoryIds: readonly string[]): void {
    lastIds = repositoryIds;
    if (reading) return;
    const unknown = unknownOf(repositoryIds);
    const retryDue = retryAt !== null && now() >= retryAt;
    if (tried !== null && !retryDue && (unknown === "" || unknown === tried)) return;
    start(unknown);
  }

  function refresh(): void {
    if (!reading) start(unknownOf(lastIds));
  }

  return { subscribe: store.subscribe, status: { subscribe: status.subscribe }, want, refresh };
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
