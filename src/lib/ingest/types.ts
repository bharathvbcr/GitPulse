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
   * Calls whose worktree had no room in this pass and were recorded under the
   * deepest worktree that did. Zero for any real family.
   */
  rehosted_calls?: number;
  reflog_entries: number;
  /** True when the wall-clock deadline stopped the pass early. */
  truncated: boolean;
  /** Ledger events the retention policy deleted at the end of the pass. */
  pruned?: number;
  error: string;
}
