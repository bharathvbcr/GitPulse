/**
 * The archive: what puts a task in it, what takes one out, and how the board
 * says so.
 *
 * There is no `archived` flag to read. `dc-store` is vendored from upstream
 * DevCouncil (`src-tauri/vendored/VENDOR.json` — change it there and
 * re-vendor, never here), its `items.list` accepts exactly `limit`, `cursor`,
 * `workspace_id`, `repository_id`, `status` and `query`, its `items.put`
 * accepts exactly nineteen named fields, and both reject any other field. So
 * "archived" is the `done` status and nothing else, and this module is the
 * one place that says so: the dock, the board's note, the card menu's Archive
 * row and the restore menu all derive from `ARCHIVE_STATUS` rather than
 * spelling "done" again.
 *
 * **This module is the seam.** Making archived a dimension of its own — a
 * task that is Done but not archived, or archived out of Ready — needs a
 * stored field the vendored store does not have. When that lands upstream,
 * `isArchived`, `archiveAction`, `restoreAction` and the dock's single
 * `listTasks` call are the whole change; nothing else asks what "archived"
 * means. [docs/ARCHIVE_SEPARATION.md](../../../docs/ARCHIVE_SEPARATION.md)
 * carries the upstream diff and the migration.
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

/** The status the archive holds. Archived means exactly this. */
export const ARCHIVE_STATUS: TaskStatus = "done";

/**
 * What puts a task in the archive, in one sentence.
 *
 * The dock says this whether or not it is empty. The rule used to appear only
 * in the empty state, which is the one moment a reader has no archived tasks
 * to wonder about: everybody with completed work saw a panel called Archive,
 * a Restore button, and nothing anywhere that said how a task gets in.
 *
 * Built from `STATUS_LABELS[ARCHIVE_STATUS]` rather than written out, so the
 * sentence cannot keep naming a column the board has renamed.
 */
export const ARCHIVE_RULE = `A task is archived when it reaches ${STATUS_LABELS[ARCHIVE_STATUS]}.`;

/**
 * Whether a task is in the archive.
 *
 * Takes the one field it reads rather than a whole `TaskCard`, so the board,
 * the menu and the dock can all ask without agreeing on a card shape first —
 * and so this stays the only place that knows which field decides it.
 */
export function isArchived(task: { status: TaskStatus }): boolean {
  return task.status === ARCHIVE_STATUS;
}

/** How much of a selection is already archived. */
export type ArchiveState = "none" | "some" | "all";

/**
 * Whether archiving a selection would do anything.
 *
 * `"all"` is the case a menu must not offer: every one of those writes would
 * spend a revision to store the value already there, and the board's own
 * convention is that a row showing the current value is disabled rather than
 * hidden. `"some"` is offered — archiving the rest is a real change.
 */
export function archiveState(tasks: readonly { status: TaskStatus }[]): ArchiveState {
  if (!tasks.length) return "none";
  const archived = tasks.filter(isArchived).length;
  if (archived === 0) return "none";
  return archived === tasks.length ? "all" : "some";
}

/**
 * The part of a selection that archiving would actually change.
 *
 * A mixed selection is offered, so the board has to be able to act on it
 * without touching the tasks that are already there. Writing all of them
 * spends a revision per already-archived task to store the value it already
 * has, and hands every one of those writes its own chance to fail — the same
 * waste a Move row showing the current status is disabled to avoid.
 *
 * Order is preserved, so a receipt reads in the order the reader selected.
 */
export function archivable<T extends { status: TaskStatus }>(tasks: readonly T[]): T[] {
  return tasks.filter((task) => !isArchived(task));
}

/**
 * Archiving, as an action.
 *
 * The mirror of `restoreAction`, and the same kind of thing: the ordinary
 * `TaskBatch` status update the board's Move menu builds, run through the
 * board's existing confirm-and-retry path. A private write here would be a
 * second owner of "move a task" that no longer shares the board's revision
 * checks, uncertainty handling or receipt identity.
 *
 * Takes no argument because there is exactly one archive. When `archived`
 * becomes its own stored field this returns `{ archived: true }` instead and
 * every caller is unchanged.
 */
export function archiveAction(): TaskAction {
  return { kind: "update", changes: { status: ARCHIVE_STATUS } };
}

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
  // Short on purpose. `ARCHIVE_RULE` now states what the archive holds, so
  // these say only the thing the rule does not: whether the same tasks are
  // also drawn on the board right now. Every character here is fixed chrome
  // above the scrolling list, and a wrapped second line costs the list a row.
  return onBoard
    ? {
        onBoard,
        sentence: "Also in the Done column on the board.",
        actionLabel: "Hide Done on the board",
      }
    : {
        onBoard,
        sentence: "The Done column is hidden.",
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
