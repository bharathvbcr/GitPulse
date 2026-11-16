import { debounce, type Debounced } from "../async/debounce";
import { invalidateSharedReads } from "../ipc/invoke";
import { createForegroundBinding, readHiddenDocument } from "../runtime/foreground";
import type { InvokeFn } from "../stores/graphStore";
import type { RepoChange } from "./events";
import { facetForChange, mergeFacets, type HydrationFacet } from "./hydration";
import { identityKey, sameRepo, type PathIdentityOptions } from "./paths";
import type { createSessionBook } from "./sessionBook";
import type { RepoSession } from "./storeTypes";
import { createWatchPool } from "./watchPool";
import { WATCH_ACTIVE, WATCH_PARKED, watchFailed, type WatchState } from "./watchState";
import { createWatcherRefreshPolicy, WATCHER_REFRESH_DEBOUNCE_MS } from "./watcherRefresh";

/**
 * Watcher events for a repo are dropped for this long after one of our own
 * mutations succeeds there: they are echoes of the mutation's own `.git`
 * writes, and honoring them means every mutation costs TWO full refreshes.
 */
export const WATCHER_ECHO_SUPPRESS_MS = 2_500;

export type WatchSessionPatch =
  | Partial<RepoSession>
  | ((current: RepoSession) => Partial<RepoSession>);

export interface WatchCoordinatorDeps {
  invoke: InvokeFn;
  options: PathIdentityOptions;
  sessionBook: ReturnType<typeof createSessionBook>;
  sessionById(id: string): RepoSession | undefined;
  activeSession(): RepoSession | undefined;
  findSessionByPath(repoPath: string): RepoSession | undefined;
  applyToSession(id: string, generation: number, patchOrFn: WatchSessionPatch): boolean;
  refresh(repoPath?: string, facet?: HydrationFacet): Promise<void>;
  holdsPath(path: string): boolean;
  isHidden?: () => boolean;
  now?: () => number;
}

/**
 * Coordinates the native filesystem watch pool, per-tab watch serialization,
 * echo suppression for our own mutations, and visibility-aware watcher
 * refresh throttling.
 */
export function createWatchCoordinator(deps: WatchCoordinatorDeps) {
  const { invoke: invokeFn, options, sessionBook } = deps;
  const { mutationEchoUntil } = sessionBook;
  const now = deps.now ?? (() => Date.now());
  const isHidden = deps.isHidden ?? readHiddenDocument;

  const watchPool = createWatchPool();
  const watchOps = new Map<string, Promise<void>>();
  const watcherRefreshTimers = new Map<string, Debounced<[]>>();
  const pendingFacets = new Map<string, HydrationFacet>();
  const droppedWhileHeld = new Set<string>();

  async function watch(path: string): Promise<WatchState> {
    try {
      await invokeFn("cmd_watch_repo", { repoPath: path });
      return WATCH_ACTIVE;
    } catch (err: unknown) {
      return watchFailed(err);
    }
  }

  async function noteTabActivated(path: string): Promise<void> {
    try {
      await invokeFn("cmd_note_tab_activated", { repoPath: path });
    } catch {
      /* reported by the hydrate that follows */
    }
  }

  function serialWatch(id: string, op: () => Promise<void>): Promise<void> {
    const run = (watchOps.get(id) ?? Promise.resolve()).then(op);
    const tail = run.catch(() => {});
    watchOps.set(id, tail);
    void tail.then(() => {
      if (watchOps.get(id) === tail) watchOps.delete(id);
    });
    return run;
  }

  function setWatch(id: string, state: WatchState): void {
    const session = deps.sessionById(id);
    if (session) deps.applyToSession(id, session.generation, { watch: state });
  }

  function admitWatch(id: string, foreground: boolean) {
    for (const key of watchPool.keys()) {
      if (!deps.sessionById(key) && key !== id) watchPool.release(key);
    }
    return watchPool.admit(id, foreground);
  }

  function hasWatch(id: string): boolean {
    return watchPool.has(id);
  }

  const watcherRefreshPolicy = createWatcherRefreshPolicy({
    now,
    setTimer: (run, delayMs) => setTimeout(run, delayMs),
    clearTimer: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    isActive: (path) => {
      const active = deps.activeSession();
      return !!active && sameRepo(active.path, path, options);
    },
    isHidden,
    refresh: (path) => {
      const facet = pendingFacets.get(path) ?? "all";
      pendingFacets.delete(path);
      return deps.refresh(path, facet);
    },
  });

  const refreshForeground = createForegroundBinding(() =>
    watcherRefreshPolicy.onVisibilityChange(),
  );

  async function parkWatch(id: string): Promise<void> {
    const path = deps.sessionById(id)?.path;
    if (!path || watchPool.has(id)) return;
    setWatch(id, WATCH_PARKED);
    watcherRefreshPolicy.forget(path);
    await serialWatch(id, async () => {
      if (!watchPool.has(id)) await unwatchNative(path);
    });
  }

  async function acquireWatch(id: string, path: string, foreground: boolean): Promise<void> {
    const { admitted, evicted } = admitWatch(id, foreground);
    if (!admitted) {
      setWatch(id, WATCH_PARKED);
      return;
    }
    const admission = serialWatch(id, async () => {
      if (!watchPool.has(id)) return;
      const state = await watch(path);
      if (watchPool.has(id)) setWatch(id, state);
    });
    if (evicted) void parkWatch(evicted);
    await admission;
  }

  async function unwatchNative(path: string): Promise<void> {
    try {
      await invokeFn("cmd_unwatch_repo", { repoPath: path });
    } catch {
      /* unwatch is best-effort */
    }
  }

  async function unwatch(path: string): Promise<void> {
    watcherRefreshPolicy.forget(path);
    const id = identityKey(path, options);
    watchPool.release(id);
    await serialWatch(id, () => unwatchNative(path));
  }

  function scheduleWatcherRefresh(key: string, run: () => void): void {
    let timer = watcherRefreshTimers.get(key);
    if (!timer) {
      timer = debounce(run, WATCHER_REFRESH_DEBOUNCE_MS);
      watcherRefreshTimers.set(key, timer);
    }
    timer();
  }

  function markEchoSuccess(path: string): void {
    mutationEchoUntil.set(path, now() + WATCHER_ECHO_SUPPRESS_MS);
  }

  function isEchoOfOwnWrite(path: string): boolean {
    if (deps.holdsPath(path)) {
      droppedWhileHeld.add(path);
      return true;
    }
    const until = mutationEchoUntil.get(path);
    return until !== undefined && now() < until;
  }

  function replayDroppedEchoes(path: string, refreshed: boolean): void {
    if (deps.holdsPath(path) || !droppedWhileHeld.delete(path) || refreshed) return;
    const session = deps.findSessionByPath(path);
    if (!session || session.trustRequired) return;
    refreshForeground.bind();
    watcherRefreshPolicy.onChange(session.path);
  }

  function refreshOwedFor(path: string): boolean {
    return watcherRefreshPolicy
      .owed()
      .some((key) => sameRepo(key, path, options));
  }

  function onActivated(path: string): void {
    watcherRefreshPolicy.onActivated(path);
  }

  async function handleRepoChanged(
    changedPath?: string | null,
    change?: RepoChange | null,
  ): Promise<void> {
    invalidateSharedReads();
    if (!changedPath) {
      if (deps.activeSession()?.trustRequired) return;
      scheduleWatcherRefresh("", () => void deps.refresh(undefined, "all"));
      return;
    }
    const session = deps.findSessionByPath(changedPath);
    if (!session || session.trustRequired) return;
    if (isEchoOfOwnWrite(session.path)) return;

    const incomingFacet = facetForChange(change);
    const existingFacet = pendingFacets.get(session.path);
    pendingFacets.set(session.path, mergeFacets(existingFacet, incomingFacet));

    refreshForeground.bind();
    watcherRefreshPolicy.onChange(session.path);
  }

  function markRepoGone(gonePath: string): void {
    const session = deps.findSessionByPath(gonePath);
    if (!session || session.missing) return;
    deps.applyToSession(session.id, session.generation, {
      missing: true,
      isLoading: false,
      error: `This checkout no longer exists at ${session.path}. Close the tab, or recreate the checkout and refresh.`,
    });
  }

  return {
    watch,
    noteTabActivated,
    admitWatch,
    hasWatch,
    acquireWatch,
    unwatch,
    markEchoSuccess,
    replayDroppedEchoes,
    refreshOwedFor,
    onActivated,
    handleRepoChanged,
    markRepoGone,
  };
}
