/**
 * Work-tree freshness for agent-driven sessions.
 *
 * The `.git` watcher catches commits, stashes and checkouts, but an agent
 * editing files only changes the working tree — nothing under `.git` moves
 * until something stages. A light `git status` poll closes that gap: one fast
 * subprocess per tick, skipped whenever the window is hidden, a load is
 * already running, or the previous poll has not landed yet.
 */

export const STATUS_POLL_INTERVAL_MS = 6_000;

export interface PollGateInput {
  /** Document.hidden — background windows must not spend subprocesses. */
  hidden: boolean;
  hasSession: boolean;
  /** A hydrate or mutation refresh is already loading this session. */
  isLoading: boolean;
  /** The previous poll's invoke has not resolved yet. */
  inflight: boolean;
}

export function shouldRunStatusPoll(input: PollGateInput): boolean {
  if (!input.hasSession || input.hidden || input.isLoading || input.inflight) {
    return false;
  }
  return true;
}
