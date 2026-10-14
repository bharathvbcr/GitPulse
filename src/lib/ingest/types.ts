/**
 * Wire types for catch-up replay of external history.
 *
 * Replays agent transcripts (~/.claude/projects/) and git's own reflog
 * on repo open so GitPulse captures what occurred while closed.
 *
 * Mirrors `src-tauri/src/ingest/mod.rs`.
 */
export interface CatchUp {
  recorded: number;
  transcripts: number;
  /**
   * Transcript files the corpus walk listed. A pass the deadline cut short
   * reads fewer than it listed, so both numbers are carried.
   */
  transcripts_listed?: number;
  skipped_lines: number;
  /**
   * Calls whose worktree had no slot in this pass (past the participant cap).
   * The row is still the owner's, carried on another worktree's transaction.
   * Zero for any real family.
   */
  rehosted_calls?: number;
  /**
   * Calls from removed worktrees past the per-pass bound on new ones: recorded
   * under the removed worktree's path, but its own cursor was not written.
   */
  departed_overflow?: number;
  /**
   * True when the worktree family was not resolved this cycle. For a Git
   * checkout the transcript pass was skipped (no row written, no cursor moved,
   * `error` says why) and the next cycle catches up; a directory that is not a
   * Git checkout still got its pass for itself alone.
   */
  family_unresolved?: boolean;
  /**
   * Consecutive cycles a Git checkout's transcript pass was skipped because its
   * family could not be resolved. Zero once it resolves; a number that only
   * grows is a family that never will.
   */
  family_unresolved_cycles?: number;
  reflog_entries: number;
  /** True when the wall-clock deadline stopped the pass early. */
  truncated: boolean;
  /** Ledger events the retention policy deleted at the end of the pass. */
  pruned?: number;
  error: string;
}
