/**
 * Wire types for the merge-conflict document.
 *
 * These lived in ConflictEditor's module script under shorter names, with a
 * third structural copy in its test file — so neither could fail on drift.
 * They carry their Rust names here, which is what puts them under
 * `check:types`, and that comparison immediately showed the copies had been
 * missing the per-line CRLF fields Rust has always sent.
 */

/** Mirrors the Rust `ConflictResolutionChoice` enum's serde form: unit
 * variants as bare strings, `Custom` as a single-key object. */
export type ConflictResolutionChoice =
  | "Unresolved"
  | "AcceptOurs"
  | "AcceptTheirs"
  | "AcceptBothOursFirst"
  | "AcceptBothTheirsFirst"
  | { Custom: string };

export interface ConflictChunk {
  chunk_index: number;
  start_line: number;
  end_line: number;
  ours_label: string;
  ours_content: string;
  /** Absent in the two-way format; `null` on the wire, not undefined. */
  base_content?: string | null;
  theirs_label: string;
  theirs_content: string;
  resolution: ConflictResolutionChoice;
  /** Per-line CRLF flags parallel to `ours_content`'s lines. */
  ours_crlf: boolean[];
  /** Per-line CRLF flags parallel to `theirs_content`'s lines. */
  theirs_crlf: boolean[];
  /** Per-line CRLF flags parallel to `base_content`'s lines. */
  base_crlf?: boolean[] | null;
  /** The file's local EOL convention around this conflict. */
  local_crlf: boolean;
}

/** One serde-externally-tagged `FileSegment` variant. */
export type FileSegment = { Normal?: string; Conflict?: ConflictChunk };

export interface ConflictDocument {
  file_path: string;
  segments: FileSegment[];
  total_conflicts: number;
  /** The file contains CRLF somewhere. */
  crlf: boolean;
  /** The file ended with a newline; render_resolved preserves that. */
  trailing_newline: boolean;
  /** Terminator kind (true = CRLF) of the file's final line. */
  final_crlf: boolean;
  /** Per-Normal-segment per-line CRLF flags, parallel to `segments`. */
  normal_crlf_flags: boolean[][];
}
