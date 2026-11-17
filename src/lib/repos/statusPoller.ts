import { diagnostics } from "../diagnostics/diagnostics";
import type { InvokeFn } from "../stores/graphStore";
import { createForegroundBinding, readBackgroundDocument } from "../runtime/foreground";
import { decideCadence, readEventLoopDelay, type CadenceDecision } from "../runtime/loadCadence";
import { formatErrorRaw } from "../ui/formatError";
import type { RepoChange } from "./events";
import {
  changeFromStatusPoll,
  shouldRunStatusPoll,
  statusPollRequest,
  statusPollRestMs,
  statusesEqual,
} from "./statusPoll";
import type { FileStatus, RepoSession } from "./storeTypes";
import { needsFullPoll } from "./watchState";

/**
 * Consecutive failed status polls before the bar says the file list may be
 * stale. One failure is routine (a repo mid-rename, a deferral); three in a
 * row at the poll's rest is about eighteen seconds of silence.
 */
export const STATUS_UNAVAILABLE_AFTER_FAILURES = 3;

/**
 * How many poll ticks between re-asserting the active repository's watch.
 *
 * A watch can die AFTER it was established — the backend reaps a session
 * whose event stream closes, whose thread panics, or whose repository
 * disappears. Re-asserting repairs rather than merely reports: `cmd_watch_repo`
 * returns immediately when the session is still registered, and creates a fresh
 * watcher when it was reaped.
 */
export const WATCH_REASSERT_EVERY_MS = 60_000;

export type SessionPatchOrFn =
  | Partial<RepoSession>
  | ((current: RepoSession) => Partial<RepoSession>);

export interface StatusPollerDeps {
  invoke: InvokeFn;
  activeSession(): RepoSession | undefined;
  sessionById(id: string): RepoSession | undefined;
  applyToSession(id: string, generation: number, patchOrFn: SessionPatchOrFn): boolean;
  snapshotRunOf(id: string): number | undefined;
  getPollRun(id: string): number | undefined;
  setPollRun(id: string, run: number): void;
  deletePollRun(id: string): void;
  /** The poll's read is about to be applied; a snapshot read started earlier is now older than it. */
  notePollApplied(id: string): void;
  acquireWatch(id: string, path: string, foreground: boolean): Promise<void>;
  hydrate(id: string, path: string, generation: number): Promise<void>;
  decideCadence?: (input: { baseMs: number; lagMs: number; paused: boolean }) => CadenceDecision;
  isBackground?: () => boolean;
  now?: () => number;
}

/**
 * Coordinates the single workspace work-tree status poll timer, foreground
 * pause/resume binding, degraded-watch full hydration, and stale-status
 * failure tracking.
 */
export function createStatusPoller(deps: StatusPollerDeps) {
  const { invoke: invokeFn } = deps;
  const now = deps.now ?? (() => Date.now());
  const isBackground = deps.isBackground ?? readBackgroundDocument;
  const cadence = deps.decideCadence ?? decideCadence;

  let timer: ReturnType<typeof setTimeout> | null = null;
  let wanted = false;
  let inflight = false;
  let sequenceSource = 0;
  let lastWatchReassertAt = now();
  let lastDurationMs: number | null = null;

  const polledChangeListeners = new Set<(path: string, change: RepoChange) => void>();

  const foregroundBinding = createForegroundBinding(() => {
    if (!wanted) return;
    if (isBackground()) {
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      return;
    }
    arm();
  });

  function arm(): void {
    if (!wanted || timer !== null || typeof setTimeout === "undefined") return;
    const decision = cadence({
      baseMs: statusPollRestMs({ lastDurationMs }),
      lagMs: readEventLoopDelay(),
      paused: isBackground(),
    });
    if (!decision.run) return;
    timer = setTimeout(() => {
      timer = null;
      void run().finally(() => {
        if (wanted) arm();
      });
    }, decision.delayMs);
  }

  function rearm(): void {
    if (!wanted || timer === null) return;
    clearTimeout(timer);
    timer = null;
    arm();
  }

  function ensure(): void {
    if (wanted || typeof setTimeout === "undefined") return;
    wanted = true;
    foregroundBinding.bind();
    arm();
  }

  function stop(): void {
    wanted = false;
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    foregroundBinding.unbind();
  }

  function resetDuration(): void {
    lastDurationMs = null;
  }

  function onPolledChange(listener: (path: string, change: RepoChange) => void): () => void {
    polledChangeListeners.add(listener);
    return () => void polledChangeListeners.delete(listener);
  }

  async function run(): Promise<void> {
    const session = deps.activeSession();
    const hidden = isBackground();
    if (
      !shouldRunStatusPoll({
        hidden,
        hasSession: Boolean(session),
        isLoading: Boolean(session?.isLoading),
        inflight,
      })
    ) {
      return;
    }
    if (session!.trustRequired) return;
    const path = session!.path;
    const generation = session!.generation;
    const sessionId = session!.id;

    if (now() - lastWatchReassertAt >= WATCH_REASSERT_EVERY_MS) {
      lastWatchReassertAt = now();
      void deps.acquireWatch(sessionId, path, true);
    }

    if (needsFullPoll(session!.watch)) {
      inflight = true;
      const startedAt = now();
      try {
        await deps.hydrate(sessionId, path, generation);
      } finally {
        inflight = false;
        lastDurationMs = now() - startedAt;
      }
      return;
    }

    sequenceSource += 1;
    const currentRun = sequenceSource;
    const snapshotRunAtStart = deps.snapshotRunOf(sessionId);
    deps.setPollRun(sessionId, currentRun);
    inflight = true;
    const startedAt = now();
    try {
      let statuses: FileStatus[];
      try {
        statuses = await invokeFn<FileStatus[]>("cmd_get_status", statusPollRequest(path));
      } catch (error) {
        const failed =
          deps.sessionById(sessionId)?.statusPollFailures ?? STATUS_UNAVAILABLE_AFTER_FAILURES;
        if (
          deps.getPollRun(sessionId) === currentRun &&
          deps.snapshotRunOf(sessionId) === snapshotRunAtStart &&
          failed < STATUS_UNAVAILABLE_AFTER_FAILURES
        ) {
          const failures = failed + 1;
          const noticed = failures === STATUS_UNAVAILABLE_AFTER_FAILURES;
          const applied = deps.applyToSession(sessionId, generation, {
            statusPollFailures: failures,
            ...(noticed ? { statusUnavailableSince: now() } : {}),
          });
          if (applied && noticed) {
            diagnostics.warn(
              "repo",
              `git status for ${path} failed ${failures} polls in a row; the file list may be stale: ${formatErrorRaw(error)}`,
            );
          }
        }
        return;
      }
      try {
        lastDurationMs = now() - startedAt;
        if (deps.getPollRun(sessionId) !== currentRun) return;
        if (deps.snapshotRunOf(sessionId) !== snapshotRunAtStart) return;
        deps.notePollApplied(sessionId);
        deps.applyToSession(sessionId, generation, (live) =>
          live.statusPollFailures === 0 && live.statusUnavailableSince === null
            ? {}
            : { statusPollFailures: 0, statusUnavailableSince: null },
        );
        const live = deps.sessionById(sessionId);
        if (
          live &&
          live.generation === generation &&
          statusesEqual(statuses, live.statuses)
        ) {
          return;
        }
        const before = live && live.generation === generation ? live.statuses : [];
        deps.applyToSession(sessionId, generation, { statuses });
        const polled = changeFromStatusPoll(before, statuses);
        if (polled) for (const listener of polledChangeListeners) listener(path, polled);
      } catch (error) {
        diagnostics.warn("repo", `publishing the status of ${path} failed: ${formatErrorRaw(error)}`);
      }
    } finally {
      if (deps.getPollRun(sessionId) === currentRun) deps.deletePollRun(sessionId);
      inflight = false;
    }
  }

  return {
    arm,
    rearm,
    ensure,
    stop,
    run,
    resetDuration,
    onPolledChange,
  };
}
