/**
 * One merge for a paged list that is also re-read while the reader pages it.
 *
 * The Agents pane polls its first page every second or so while an attempt is
 * running, and "Load more" appends older pages. Replacing the list with the
 * fresh first page on every poll threw away everything the reader had paged
 * in, so the history could not be read down while anything was live. A head
 * refresh therefore folds the fresh page into what is already loaded; it
 * never shrinks the list on its own.
 */

export interface MergeOptions<T> {
  /** True for a "Load more" page; false for a re-read of the head. */
  more: boolean;
  /**
   * Head refresh only: loaded rows the fresh read proves are no longer what
   * they were (a request that is not pending any more, an attempt that no
   * longer holds its checkout). Called for loaded rows the fresh page does not
   * contain; those it returns true for are dropped rather than shown stale.
   * Ignored for a "Load more" page, which says nothing about the head.
   */
  dropStale?: (row: T) => boolean;
}

/**
 * Folds `fresh` into `loaded`, keyed by `id`.
 *
 * - A fresh row always wins over a loaded row with the same id.
 * - `more: true` keeps the loaded rows in place and appends the fresh ones
 *   after them (older pages continue the list).
 * - `more: false` puts the fresh head first and keeps the loaded rows it does
 *   not contain after it, so a poll cannot discard the tail the reader paged
 *   in. `dropStale` removes only the absent rows it names.
 */
export function mergeLoadedPage<T extends { id: string }>(loaded: readonly T[], fresh: readonly T[], opts: MergeOptions<T>): T[] {
  const byId = new Map<string, T>();
  if (opts.more) {
    for (const row of loaded) byId.set(row.id, row);
    for (const row of fresh) byId.set(row.id, row);
    return [...byId.values()];
  }
  for (const row of fresh) byId.set(row.id, row);
  for (const row of loaded) {
    if (byId.has(row.id)) continue;
    if (opts.dropStale?.(row)) continue;
    byId.set(row.id, row);
  }
  return [...byId.values()];
}
