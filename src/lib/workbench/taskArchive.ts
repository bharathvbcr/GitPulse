/**
 * The archive: where completed tasks go, and how the board says so.
 *
 * There is no `archived` flag to read. `dc-store` is vendored from upstream
 * DevCouncil (`src-tauri/vendored/VENDOR.json` — change it there and
 * re-vendor, never here), its `items.list` accepts exactly `limit`, `cursor`,
 * `workspace_id`, `repository_id`, `status` and `query`, and it rejects any
 * other field. So "completed" is the `done` status and nothing else, and this
 * module is the one place that says so: the dock, the board's note and the
 * restore menu all derive from `ARCHIVE_STATUS` rather than spelling "done"
 * again.
 *
 * Two honesty rules run through the file, both of them the same rule the
 * board already follows for a hidden column:
 *
 * * **A page is never presented as the whole.** `archiveSummary` carries the
 *   loaded count and the server's total together, always, so a reader who has
 *   seen 30 of 412 completed tasks cannot mistake that for all of them.
 * * **The dock never claims to have emptied the board.** The archive is a
 *   second way to reach completed work, not a move; `boardPresence` says
 *   plainly whether the Done column is still drawn and offers the toggle
 *   that changes it.
 *
 * What this module deliberately does *not* do is order by completion time.
 * A `TaskCard` carries `updated_at` (last write) and no completion stamp, and
 * `items.list` orders by `(position, id)` with no sort parameter. Sorting the
 * loaded page by `updated_at` would read as "most recently completed first"
 * while page two silently held newer work, so the archive keeps the store's
 * order and labels the timestamp for what it is.
 */

import { plural } from "../format";
import type { TaskAction } from "./taskActions";
import { STATUSES, STATUS_LABELS, type TaskStatus } from "./vocabulary";

/** The status the archive holds. Completed means exactly this. */
export const ARCHIVE_STATUS: TaskStatus = "done";

/**
 * Where a completed task can be restored to, in board order.
 *
 * Derived from `STATUSES` rather than listed, so a status added upstream
 * becomes a restore target without a second edit here — and `done` can never
 * appear in its own restore menu.
 */
export const RESTORE_STATUSES: readonly TaskStatus[] = STATUSES.filter((status) => status !== ARCHIVE_STATUS);

/**
 * Restoring is an ordinary status change, so it is the ordinary status
 * change: the same `TaskBatch` update the board's Move menu builds, run
 * through the same confirm-and-retry dialog. A private write path here would
 * be a second owner of "move a task" that no longer shares the board's
 * revision checks, uncertainty handling or receipt identity.
 *
 * Refuses `done`, which would be a no-op dressed up as a restore.
 */
export function restoreAction(status: TaskStatus): TaskAction {
  if (!RESTORE_STATUSES.includes(status)) {
    const name = STATUS_LABELS[status] ? `${STATUS_LABELS[status]}.` : "that status.";
    throw new Error(`Cannot restore a completed task to ${name}`);
  }
  return { kind: "update", changes: { status } };
}

export interface ArchiveSummary {
  /** Sentence for the dock's status line. Never claims more than it loaded. */
  text: string;
  /** True while the loaded rows are only part of the server's count. */
  partial: boolean;
  /**
   * True when no read has succeeded yet, so the dock knows to hold its
   * empty state back. The distinction exists because collapsing it is the
   * failure this function is written to prevent.
   */
  pending: boolean;
}

/**
 * "Showing 30 of 412 completed tasks" — the dock's one status line.
 *
 * `loaded` is the number of rows on screen, or **null** when no read has
 * succeeded: the dock defers while its window is in the background, the
 * first request is briefly in flight, and a read can fail outright. `total`
 * is the server's count for this scope and search.
 *
 * Two things this must never do, and they are the reason it is a function
 * with tests rather than a template expression:
 *
 * * Treat "nothing read yet" as "nothing there". A dock that has not run its
 *   query saying "No completed tasks in this scope" is a check that could not
 *   run reporting the same answer as one that ran and passed — and it sat
 *   under a header badge reading 34 until the null case was separated out.
 * * Print the loaded count alone while the server's total is larger. The two
 *   numbers travel together for as long as they differ.
 */
export function archiveSummary(loaded: number | null, total: number): ArchiveSummary {
  if (loaded === null) return { text: "Completed tasks have not loaded yet.", partial: false, pending: true };
  const shown = Math.max(0, Math.trunc(loaded));
  const all = Math.max(shown, Math.trunc(total));
  if (all === 0) return { text: "No completed tasks in this scope.", partial: false, pending: false };
  if (shown >= all) return { text: `${plural(all, "completed task")}.`, partial: false, pending: false };
  return { text: `Showing ${shown} of ${plural(all, "completed task")}.`, partial: true, pending: false };
}

export interface BoardPresence {
  /** Whether the board still draws the Done column beside the archive. */
  onBoard: boolean;
  /** What that means, for the dock's footer. */
  sentence: string;
  /** The toggle that changes it, named for what it will do. */
  actionLabel: string;
}

/**
 * Whether completed tasks are still on the board, and how to change that.
 *
 * The archive does not own a second hiding mechanism. `taskHiddenColumns` is
 * already the board's one owner of "which columns are drawn", already
 * persisted, already reported by `hiddenColumnReport`, and already refuses to
 * hide every column at once. The dock reads it and offers its toggle; it does
 * not keep a competing flag that could disagree with the board.
 */
export function boardPresence(hidden: readonly TaskStatus[]): BoardPresence {
  const onBoard = !hidden.includes(ARCHIVE_STATUS);
  return onBoard
    ? {
        onBoard,
        sentence: "These tasks are also in the Done column on the board.",
        actionLabel: "Hide Done on the board",
      }
    : {
        onBoard,
        sentence: "The Done column is hidden, so this is where completed work is read.",
        actionLabel: "Show Done on the board",
      };
}

/**
 * Whether the board's hidden-column note should offer the archive.
 *
 * The note names every hidden column that holds work; only the completed one
 * has somewhere else to be read, so the extra action appears only then.
 */
export function offersArchive(hidden: readonly TaskStatus[]): boolean {
  return hidden.includes(ARCHIVE_STATUS);
}
