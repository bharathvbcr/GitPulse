/**
 * What an agent attempt's branch looks like and how it is landed.
 *
 * Its own module, like `liveRun.ts`, because the Worktrees panel (mounted at
 * startup) needs only this and importing `attemptWorktree.ts` would put that
 * module, and the task board's `taskSessions.ts` behind it, in the entry chunk.
 */

/** Branches the host cuts for an agent attempt. */
export const ATTEMPT_BRANCH_PREFIX = "gitpulse/";

/**
 * An attempt's branch is a scratch lane: its commits are the agent's working
 * steps, so landing it as one commit is the default. Any other branch keeps
 * the history it has unless the reader asks otherwise.
 */
export function squashesByDefault(branch: string | null | undefined): boolean {
  return branch?.startsWith(ATTEMPT_BRANCH_PREFIX) ?? false;
}
