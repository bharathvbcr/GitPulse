import type { WorktreeUsage } from "./types";

/** One linked worktree as `cmd_storage_stale_worktrees` reports it. */
export interface StaleWorktree {
  path: string;
  name: string;
  branch: string | null;
  /** Processes working inside it; null when the process table could not be read. */
  processes: number | null;
  /** `git status` printed nothing; null when status could not run. */
  clean: boolean | null;
  /** HEAD is reachable from the main worktree's branch; null when that could not be asked. */
  integrated: boolean | null;
  /** Whole days since its HEAD commit; null when unread. */
  idle_days: number | null;
  locked: boolean;
  /** Every check was answered and all of them hold. */
  stale: boolean;
  /** Why it is not stale, one clause per failed or unanswered check. */
  reasons: string[];
  /** Sized for stale worktrees only. */
  size: WorktreeUsage | null;
}

export interface StaleWorktreeReport {
  repo_path: string;
  idle_days: number;
  /** Linked worktrees the repository lists. */
  listed: number;
  /** Of those, how many were examined. */
  examined: number;
  worktrees: StaleWorktree[];
  issues: string[];
}
