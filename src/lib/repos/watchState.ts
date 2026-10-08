/**
 * Whether a repository is receiving live filesystem updates.
 *
 * `cmd_watch_repo` can fail for reasons that are ordinary rather than
 * exceptional: the watch table is full (the backend caps watches, and a full
 * workspace can reach that cap), the platform refuses another inotify handle
 * (`ENOSPC` on Linux is a routine limit, not a crash), or the repository moved
 * out from under the watcher and its session was reaped.
 *
 * The frontend used to swallow every one of those. That is the failure this
 * module exists to end: **a repository with no watcher looked exactly like a
 * repository with one.** The consequence is not cosmetic. The 6-second poll
 * refreshes file statuses ONLY — branches, commits, the parked-operation
 * state and the stash stack are refreshed by the watcher and by nothing else
 * on a tab the user is already sitting on. So an unwatched repository shows a
 * stale branch, a stale graph, and a stale merge banner indefinitely, while
 * presenting as live.
 *
 * Two things follow, and both live here so they cannot drift apart: the user
 * is told, and the poll compensates.
 */

export type WatchStatus =
  /** The backend confirmed a live watch. */
  | "watching"
  /** The watch could not be established, or was lost. Reason travels with it. */
  | "degraded"
  /** No attempt has settled yet. Not an assertion either way. */
  | "unknown";

export interface WatchState {
  status: WatchStatus;
  /** Why it is degraded, in the backend's words. Null otherwise. */
  reason: string | null;
}

export const WATCH_UNKNOWN: WatchState = { status: "unknown", reason: null };
export const WATCH_ACTIVE: WatchState = { status: "watching", reason: null };

/** Builds a degraded state, never losing the reason. */
export function watchFailed(reason: unknown): WatchState {
  const text =
    reason instanceof Error
      ? reason.message
      : typeof reason === "string"
        ? reason
        : "the filesystem watcher could not be started";
  return { status: "degraded", reason: text.trim() || "unknown reason" };
}

/** True when the backend confirmed live updates for this repository. */
export function isLiveUpdating(state: WatchState): boolean {
  return state.status === "watching";
}

/**
 * Whether the status poll must do a FULL refresh for this repository instead
 * of its usual statuses-only tick.
 *
 * This is the functional half of the fix. Without it, the indicator would tell
 * the user their repository is stale without doing anything about it.
 *
 * Deliberately true for `unknown` as well as `degraded`: before the first
 * watch attempt settles, assuming live updates is the assumption that leaves a
 * user staring at stale data. The cost of being wrong is one extra snapshot.
 */
export function needsFullPoll(state: WatchState): boolean {
  return state.status !== "watching";
}

/**
 * Whether to tell the user.
 *
 * `unknown` is NOT surfaced: it is the ordinary state for the first few
 * hundred milliseconds after opening a repository, and an indicator that
 * flickers on every open trains people to ignore it.
 */
export function shouldSurface(state: WatchState): boolean {
  return state.status === "degraded";
}

/** Short marker for the status bar. Null when there is nothing to say. */
export function watchMarker(state: WatchState): string | null {
  // Two words. The status bar is a dense 24px row shared with the branch,
  // sync counts and file counts; the full explanation lives in the tooltip,
  // where there is room to say what it means and what the app is doing.
  return shouldSurface(state) ? "Not live" : null;
}

/**
 * The full explanation, for the indicator's tooltip.
 *
 * Says what is degraded, what the app is doing about it, and what the user
 * would gain by fixing it — an indicator that only announces a problem leaves
 * the reader with nowhere to go.
 */
export function describeWatch(state: WatchState): string {
  switch (state.status) {
    case "watching":
      return "This repository updates live as files change.";
    case "unknown":
      return "Still setting up live updates for this repository.";
    case "degraded":
      return (
        `GitPulse is not receiving live filesystem updates for this repository` +
        (state.reason ? ` (${state.reason})` : "") +
        `. It is refreshing on a timer instead, so changes made outside GitPulse ` +
        `may take a few seconds to appear.`
      );
  }
}

/**
 * Structural equality, for the store's publish gate.
 *
 * The watch state is rebuilt on every open and every refresh; reference
 * equality would republish the whole store to every subscriber each time.
 */
export function watchStatesEqual(a: WatchState, b: WatchState): boolean {
  return a === b || (a.status === b.status && a.reason === b.reason);
}
