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
  /**
   * Transcript files the walk saw and did not list (below its depth bound or
   * past its file cap). They were not read: non-zero sets `truncated`, and
   * `error` names the reasons. A floor when `unlisted_lower_bound`.
   */
  transcripts_unlisted?: number;
  /** Of `transcripts_unlisted`, the files below the walk's depth bound. */
  unlisted_depth?: number;
  /** Of `transcripts_unlisted`, the files past the walk's file cap. */
  unlisted_cap?: number;
  unlisted_lower_bound?: boolean;
  /**
   * Symlinks the walk refused because their target is outside the transcript
   * root. Not unread transcripts, so not in `transcripts_unlisted`.
   */
  refused_outside_corpus: number;
  /**
   * Saved transcript offsets whose file this pass's walk did not list and that
   * it did not forget (forgetting held back, or a forget that failed). Zero
   * after a pass that forgot them.
   */
  stale_cursors?: number;
  /** Whether the walk listed the whole corpus; null when no walk ran. */
  walk_complete?: boolean | null;
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
  /**
   * Earlier locations of this repository that its ledger's rows named, taken
   * over by this pass because the repository was moved or renamed. Non-empty
   * only on the one pass that did it; not an error.
   */
  reanchored_from?: string[];
  reflog_entries: number;
  /**
   * Reflog entries the ledger refused because it already held them (same
   * worktree, object, event time and ordinal), so another pass or process
   * recorded them first. Not lost, not an error: `reflog_entries` equals the
   * entries recorded here plus these plus any that failed to write.
   */
  duplicates_refused?: number;
  /**
   * Reflog entries whose ledger write failed. The pass stops at the first, so
   * everything after it in that ref is `reflog_pending`; the next pass retries
   * from the same place. Non-zero is never a complete pass.
   */
  failed_records?: number;
  /**
   * Reflog entries the pass knew of and did not record: more than a window
   * newer than the watermark, or left behind a failed write. The next passes
   * record them oldest first; non-zero sets `truncated`.
   */
  reflog_pending?: number;
  /**
   * True when a ref's log reaches past what the pass read and its watermark was
   * not in the read: older entries were not examined. Sets `truncated`.
   */
  reflog_unreached?: boolean;
  /**
   * Reflogs (worktree HEADs, the default branch) the pass did not read: the
   * per-pass bound on worktrees, a deadline, or an unreadable default branch.
   * Non-zero sets `truncated`.
   */
  reflog_refs_skipped?: number;
  /** True when the wall-clock deadline stopped the pass early. */
  truncated: boolean;
  /** Ledger events the retention policy deleted at the end of the pass. */
  pruned?: number;
  error: string;
}
