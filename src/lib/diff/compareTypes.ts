/**
 * Wire shapes of the two-commit comparison and commit-notes reads.
 *
 * Kept out of `.svelte` files (the `wire-type-locality-contract` forbids
 * snake_case payload shapes there). Mirrors `CompareFiles` and `CommitNote`
 * in `src-tauri/src/engine/git_reader.rs`.
 */
import type { CommitFileChange } from "../stores/graphStore";

/** `cmd_get_compare_files`: the files `from..to` changes. */
export interface CompareFiles {
  files: CommitFileChange[];
  /** Present and true when the list was cut at its byte budget. */
  truncated?: boolean;
  /** Why it was cut, as a clause; absent (never null) when whole. */
  truncation_reason?: string;
}

/** One row of `cmd_get_commit_notes`, in the order the ids were asked. */
export interface CommitNote {
  commit_id: string;
  /** The note's text; null when the commit carries no note. */
  note: string | null;
}
