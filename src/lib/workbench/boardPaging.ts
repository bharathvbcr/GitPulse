/**
 * Keeping what a reader has paged in across a reload.
 *
 * A column or navigator list is read a page at a time; "Load more" grows it.
 * A refresh that read only page one threw that away, so any write (an agent's
 * over MCP included) snapped a 60-card Done column back to 30 and turned the
 * board's own advice, "load its remaining tasks first", into a loop. The
 * refresh reads as many pages as were on screen, through `refreshPages`: each
 * page from where the fresh previous one now ends, so a row that moved
 * between pages is neither lost nor shown twice.
 */
import type { Page } from "./client";
import { refreshPages } from "./taskArchive";

/** Where a reload stops re-reading what was paged in; "Load more" continues from there. */
export const MAX_RELOAD_PAGES = 20;
/** Where reading a column to its end stops, so one drop never fans out without bound. */
export const MAX_COLUMN_PAGES = 100;

/** How many pages of `size` hold `count` loaded rows: at least one, at most `MAX_RELOAD_PAGES`. */
export function pagesLoaded(count: number, size: number): number {
  if (!Number.isFinite(count) || count <= 0 || size <= 0) return 1;
  return Math.min(MAX_RELOAD_PAGES, Math.max(1, Math.ceil(count / size)));
}

type Read<T> = (cursor: string | undefined) => Promise<Page<T>>;

/** The pages on screen read again, as one page: `loaded` rows decide how many. */
export async function reloadPaged<T extends { id: string }>(read: Read<T>, loaded: number, size: number): Promise<Page<T>> {
  const fresh = await refreshPages(pagesLoaded(loaded, size), read);
  return { items: fresh.items, total: fresh.total, shown: fresh.items.length, has_more: fresh.next_cursor !== null, next_cursor: fresh.next_cursor };
}

/**
 * The rest of a page's list after what is loaded, or null when the list did
 * not end within `MAX_COLUMN_PAGES`: a caller that needs the true last row
 * (the position after it) must then refuse rather than guess.
 */
export async function readToEnd<T extends { id: string }>(read: Read<T>, current: Page<T>): Promise<Page<T> | null> {
  const items = new Map(current.items.map((item) => [item.id, item]));
  let cursor = current.next_cursor, last = current;
  for (let pages = 0; cursor !== null && pages < MAX_COLUMN_PAGES; pages++) {
    last = await read(cursor);
    for (const item of last.items) items.set(item.id, item);
    cursor = last.next_cursor;
  }
  if (cursor !== null) return null;
  const all = [...items.values()];
  return { ...last, items: all, shown: all.length, has_more: false, next_cursor: null };
}
