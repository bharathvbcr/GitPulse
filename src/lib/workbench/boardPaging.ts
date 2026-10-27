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

/**
 * Whether two reads of a list hold the same records, field for field, in the
 * same order. A background refresh that found nothing new keeps the list it
 * has, so the screen is not redrawn for an answer it already shows. The
 * revision alone does not decide it: a row carries derived fields (a
 * workspace's `repository_count`) that change without its revision moving.
 */
export function sameRecords(a: readonly { id: string; revision: number }[], b: readonly { id: string; revision: number }[]): boolean {
  return a.length === b.length
    && a.every((row, i) => row.id === b[i].id && row.revision === b[i].revision && JSON.stringify(row) === JSON.stringify(b[i]));
}

/** `sameRecords` for a page, with its totals and cursor. */
export function samePage<T extends { id: string; revision: number }>(a: Page<T> | undefined, b: Page<T> | undefined): boolean {
  if (!a || !b) return a === b;
  return a.total === b.total && a.next_cursor === b.next_cursor && a.has_more === b.has_more && a.shown === b.shown && sameRecords(a.items, b.items);
}

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
 * The page size a reload reads `loaded` rows back at: `size` while
 * `MAX_RELOAD_PAGES` of it reach them, else `largest` (the store's page
 * maximum), so a column loaded past 600 cards is not cut back to 600.
 */
export function reloadSize(loaded: number, size: number, largest: number): number {
  return loaded > size * MAX_RELOAD_PAGES ? largest : size;
}

export interface ReadOn<T> { page: Page<T>; ended: boolean }

/**
 * `current` with the pages after it folded in, one request at a time (each
 * cursor comes back with the page before it), until the list ends, `maxPages`
 * were read, it holds `maxRows`, or `onPage` — shown each step as it lands —
 * returns false. `ended` says whether the list's true end was reached.
 */
export async function readOn<T extends { id: string }>(
  read: Read<T>,
  current: Page<T>,
  maxPages: number,
  { maxRows = Number.POSITIVE_INFINITY, onPage }: { maxRows?: number; onPage?: (page: Page<T>) => boolean } = {},
): Promise<ReadOn<T>> {
  const items = new Map(current.items.map((item) => [item.id, item]));
  let cursor = current.next_cursor, last = current;
  const folded = (): Page<T> => {
    const all = [...items.values()];
    return { ...last, items: all, shown: all.length, has_more: cursor !== null, next_cursor: cursor };
  };
  for (let pages = 0; cursor !== null && pages < maxPages && items.size < maxRows; pages++) {
    last = await read(cursor);
    for (const item of last.items) items.set(item.id, item);
    // A store that hands back the cursor it was given would be read forever.
    const advanced = last.next_cursor !== cursor;
    cursor = last.next_cursor;
    if ((onPage && !onPage(folded())) || !advanced) break;
  }
  return { page: folded(), ended: cursor === null };
}

/**
 * The rest of a page's list after what is loaded, or null when the list did
 * not end within `MAX_COLUMN_PAGES`: a caller that needs the true last row
 * (the position after it) must then refuse rather than guess.
 */
export async function readToEnd<T extends { id: string }>(read: Read<T>, current: Page<T>): Promise<Page<T> | null> {
  const { page, ended } = await readOn(read, current, MAX_COLUMN_PAGES);
  return ended ? page : null;
}
