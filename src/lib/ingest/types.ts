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
  skipped_lines: number;
  reflog_entries: number;
  error: string;
}
