/**
 * The archive: what puts a task in it, what takes one out, and how the dock
 * says so.
 *
 * Archived is its own stored flag (`archived` on the task, dc-store schema
 * 11), independent of status. Finishing a task leaves it in the Done column;
 * Archive files it away from whatever column it is in, and Restore brings it
 * back to the column it left, with its status untouched. This module is the
 * one place that says what archived means: the dock, the board's columns, the
 * card menu's Archive row and the selection bar all ask it.
 *
 * Two honesty rules run through the file, both of them the same rule the
 * board already follows for a hidden column:
 *
 * * **A page is never presented as the whole.** `archiveSummary` carries the
 *   loaded count and the server's total together, always, so a reader who has
 *   seen 30 of 412 archived tasks cannot mistake that for all of them.
 * * **Nothing read yet is not nothing there.** A dock that has not run its
 *   query never renders as an empty archive.
 *
 * The archive reads most recently completed first, by the store's own
 * `completed_at`. A task archived without being finished has no completion
 * time and reads after every finished one.
 */

import { plural } from "../format";
import type { TaskAction } from "./taskActions";

/**
 * What puts a task in the archive, in one sentence.
 *
 * The dock says this whether or not it is empty. A panel called Archive with
 * a Restore button and nothing that says how a task gets in leaves the reader
 * to guess — which used to be "it reached Done", and no longer is.
 */
export const ARCHIVE_RULE = "Archive files a task away from the board in any column; Restore puts it back where it was.";

/**
 * Whether a task is in the archive.
 *
 * Takes the one field it reads rather than a whole `TaskCard`, so the board,
 * the menu and the dock can all ask without agreeing on a card shape first —
 * and so this stays the only place that knows which field decides it.
 */
export function isArchived(task: { archived: boolean }): boolean {
  return task.archived === true;
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
export function archiveState(tasks: readonly { archived: boolean }[]): ArchiveState {
  if (!tasks.length) return "none";
  const archived = tasks.filter(isArchived).length;
  if (archived === 0) return "none";
  return archived === tasks.length ? "all" : "some";
}

/**
 * The part of a selection that archiving would actually change.
 *
 * Writing an already-archived task spends a revision to store the value it
 * already has and hands that write its own chance to fail. Order is
 * preserved, so a receipt reads in the order the reader selected.
 */
export function archivable<T extends { archived: boolean }>(tasks: readonly T[]): T[] {
  return tasks.filter((task) => !isArchived(task));
}

/**
 * Archiving, as an action: the ordinary `TaskBatch` update, run through the
 * board's existing confirm-and-retry path. A private write here would be a
 * second owner of "change a task" that no longer shares the board's revision
 * checks, uncertainty handling, receipt identity or undo.
 */
export function archiveAction(): TaskAction {
  return { kind: "update", changes: { archived: true } };
}

/**
 * Restoring from the archive. The task keeps the status it has: a Done task
 * returns to Done, one archived out of Ready returns to Ready. There is no
 * target to choose, so there is no way to choose a wrong one.
 */
export function restoreAction(): TaskAction {
  return { kind: "update", changes: { archived: false } };
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

/** What the dock is listing: the archive, or deleted tasks a restore can bring back. */
export type ArchiveView = "archived" | "deleted";
const NOUN: Record<ArchiveView, string> = { archived: "archived task", deleted: "deleted task" };

/**
 * "Showing 30 of 412 archived tasks" — the dock's one status line.
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
 *   query saying "No archived tasks in this scope" is a check that could not
 *   run reporting the same answer as one that ran and passed.
 * * Print the loaded count alone while the server's total is larger. The two
 *   numbers travel together for as long as they differ.
 */
export function archiveSummary(loaded: number | null, total: number, view: ArchiveView = "archived"): ArchiveSummary {
  const noun = NOUN[view];
  if (loaded === null) return { text: `${noun[0].toUpperCase()}${noun.slice(1)}s have not loaded yet.`, partial: false, pending: true };
  const shown = Math.max(0, Math.trunc(loaded));
  const all = Math.max(shown, Math.trunc(total));
  if (all === 0) return { text: `No ${noun}s in this scope.`, partial: false, pending: false };
  if (shown >= all) return { text: `${plural(all, noun)}.`, partial: false, pending: false };
  return { text: `Showing ${shown} of ${plural(all, noun)}.`, partial: true, pending: false };
}

/**
 * The stamp beside an archived row: when it was completed, which is what the
 * dock is ordered by, or — for a task archived without being finished — that
 * it never was, with its last update. Never a completion time it does not have.
 */
export function archiveStamp(task: { completed_at: number | null; updated_at: number }, relative: (at: number) => string): string {
  return task.completed_at !== null
    ? `Completed ${relative(task.completed_at)}`
    : `Not completed · updated ${relative(task.updated_at)}`;
}

/**
 * Re-read the pages already on screen, so a write refreshes the dock in place
 * instead of collapsing it to its first page.
 *
 * The store's cursor is a key, not an offset, so each page is read from where
 * the *fresh* previous page now ends rather than from the cursor it was first
 * read with: a row that moved between pages is then neither lost nor shown
 * twice. Reading stops early when the list has become shorter than the pages
 * that were loaded. `pages` is how many were loaded; at least one is read.
 */
export async function refreshPages<T extends { id: string }>(
  pages: number,
  read: (cursor: string | undefined) => Promise<{ items: T[]; total: number; next_cursor: string | null }>,
): Promise<{ items: T[]; total: number; next_cursor: string | null; pages: number }> {
  const items = new Map<string, T>();
  let total = 0, next: string | null = null, read_ = 0;
  do {
    const result = await read(next ?? undefined);
    read_++;
    for (const item of result.items) items.set(item.id, item);
    total = result.total; next = result.next_cursor;
  } while (read_ < Math.max(1, pages) && next !== null);
  return { items: [...items.values()], total, next_cursor: next, pages: read_ };
}
