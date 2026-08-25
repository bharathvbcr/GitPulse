/**
 * Outcome planning for ConflictEditor's save flow.
 *
 * The file write and the index staging are two independent steps; treating a
 * stage failure after a successful write as total failure lied twice — the
 * journal recorded an edit that did happen, and the user was told nothing
 * was saved when the working tree already held the resolution.
 */
import { formatError } from "../ui/formatError";

export interface ConflictSavePlan {
  /** The resolved content reached the working tree. */
  written: boolean;
  /** The staged index picked up the resolution. */
  staged: boolean;
  /** True only when the write and the stage both succeeded. */
  complete: boolean;
  /**
   * Journal honesty flag: the `edit` action happened exactly when the file
   * was written, regardless of what staging did afterwards.
   */
  journalOk: boolean;
  /** User-facing message; null when every step succeeded. */
  message: string | null;
}

export function planConflictSave(
  written: boolean,
  staged: boolean,
  error?: unknown
): ConflictSavePlan {
  if (!written) {
    return {
      written,
      staged,
      journalOk: false,
      complete: false,
      message: `Save failed: ${formatError(error)}`,
    };
  }
  if (!staged) {
    return {
      written,
      staged,
      journalOk: true,
      complete: false,
      message: `Resolution saved to the file, but staging failed: ${formatError(error)}`,
    };
  }
  return { written, staged, journalOk: true, complete: true, message: null };
}
