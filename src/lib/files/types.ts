/**
 * Wire types for the file-content IPC payloads.
 *
 * These were declared inside BlameViewer and DiffViewer, which put them out of
 * reach of `check:types` — it needs a module to point at, not a component. A
 * Rust field rename would have surfaced as a silently `undefined` property in
 * whichever viewer had not been updated, the same failure TerminalRunResult
 * had before it was moved out of its components.
 */

/** One line of `git blame --line-porcelain` output. */
export interface BlameLine {
  line_no: number;
  commit_id: string;
  author_name: string;
  author_email: string;
  timestamp: number;
  content: string;
}

/** A single file's contents, as text or base64 depending on `is_binary`. */
export interface FileBlob {
  path: string;
  is_binary: boolean;
  is_image: boolean;
  mime: string;
  text?: string | null;
  base64?: string | null;
}
