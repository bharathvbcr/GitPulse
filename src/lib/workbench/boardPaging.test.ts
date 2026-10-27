import { describe, expect, it } from "vitest";
import { MAX_COLUMN_PAGES, MAX_RELOAD_PAGES, pagesLoaded, readOn, readToEnd, reloadPaged, reloadSize, samePage, sameRecords } from "./boardPaging";
import type { Page } from "./client";

type Row = { id: string };
const rows = (count: number): Row[] => Array.from({ length: count }, (_, i) => ({ id: `t${String(i).padStart(4, "0")}` }));

/** A store whose cursor is the id of the last row read, as the real one's is a key and not an offset. */
function store(all: Row[], size: number) {
  const reads: (string | undefined)[] = [];
  const read = async (cursor: string | undefined): Promise<Page<Row>> => {
    reads.push(cursor);
    const start = cursor === undefined ? 0 : all.findIndex((row) => row.id === cursor) + 1;
    const items = all.slice(start, start + size);
    const more = start + size < all.length;
    return { items, total: all.length, shown: items.length, has_more: more, next_cursor: more ? items[items.length - 1].id : null };
  };
  return { read, reads };
}

describe("pagesLoaded", () => {
  it("counts the pages that hold the loaded rows, at least one", () => {
    expect(pagesLoaded(0, 30)).toBe(1);
    expect(pagesLoaded(1, 30)).toBe(1);
    expect(pagesLoaded(30, 30)).toBe(1);
    expect(pagesLoaded(31, 30)).toBe(2);
    expect(pagesLoaded(60, 30)).toBe(2);
  });
  it("is bounded, and tolerates nonsense", () => {
    expect(pagesLoaded(1_000_000, 30)).toBe(MAX_RELOAD_PAGES);
    expect(pagesLoaded(Number.NaN, 30)).toBe(1);
    expect(pagesLoaded(-5, 30)).toBe(1);
    expect(pagesLoaded(60, 0)).toBe(1);
  });
});

describe("reloadPaged", () => {
  it("keeps what was paged in: 60 loaded rows read again are 60 rows, not page one", async () => {
    const { read, reads } = store(rows(100), 30);
    const page = await reloadPaged(read, 60, 30);
    expect(page.items).toHaveLength(60);
    expect(page.shown).toBe(60);
    expect(page.total).toBe(100);
    expect(page.next_cursor).toBe("t0059");
    expect(reads).toEqual([undefined, "t0029"]);
  });
  it("reads one page for an unloaded or first-page column", async () => {
    const { read, reads } = store(rows(100), 30);
    expect((await reloadPaged(read, 0, 30)).items).toHaveLength(30);
    expect((await reloadPaged(read, 30, 30)).items).toHaveLength(30);
    expect(reads).toEqual([undefined, undefined]);
  });
  it("stops when the list became shorter than what was loaded", async () => {
    const { read, reads } = store(rows(40), 30);
    const page = await reloadPaged(read, 90, 30);
    expect(page.items).toHaveLength(40);
    expect(page.next_cursor).toBeNull();
    expect(page.has_more).toBe(false);
    expect(reads).toHaveLength(2);
  });
  it("reads an empty list as empty", async () => {
    const { read } = store([], 30);
    expect(await reloadPaged(read, 60, 30)).toEqual({ items: [], total: 0, shown: 0, has_more: false, next_cursor: null });
  });
  it("never reads more pages than the reload bound", async () => {
    const { read, reads } = store(rows(30 * (MAX_RELOAD_PAGES + 5)), 30);
    const page = await reloadPaged(read, 1_000_000, 30);
    expect(reads).toHaveLength(MAX_RELOAD_PAGES);
    expect(page.items).toHaveLength(30 * MAX_RELOAD_PAGES);
    expect(page.next_cursor).not.toBeNull();
  });
  it("propagates a failed read instead of returning a partial column", async () => {
    let calls = 0;
    const read = async (): Promise<Page<Row>> => {
      if (++calls === 2) throw new Error("store down");
      return { items: rows(30), total: 90, shown: 30, has_more: true, next_cursor: "t0029" };
    };
    await expect(reloadPaged(read, 90, 30)).rejects.toThrow("store down");
  });
});

describe("readToEnd", () => {
  it("reads the rest of a column after what is loaded, without duplicating rows", async () => {
    const all = rows(95);
    const { read, reads } = store(all, 30);
    const first = await read(undefined);
    reads.length = 0;
    const full = await readToEnd(read, first);
    expect(full?.items.map((row) => row.id)).toEqual(all.map((row) => row.id));
    expect(full?.next_cursor).toBeNull();
    expect(full?.shown).toBe(95);
    expect(reads).toEqual(["t0029", "t0059", "t0089"]);
  });
  it("is the page itself when nothing is left", async () => {
    const { read, reads } = store(rows(10), 30);
    const first = await read(undefined);
    reads.length = 0;
    const full = await readToEnd(read, first);
    expect(full?.items).toHaveLength(10);
    expect(reads).toEqual([]);
  });
  it("gives up as null when the list does not end within the bound, so nobody positions after a guess", async () => {
    const { read, reads } = store(rows(10 * (MAX_COLUMN_PAGES + 20)), 10);
    const first = await read(undefined);
    reads.length = 0;
    expect(await readToEnd(read, first)).toBeNull();
    expect(reads).toHaveLength(MAX_COLUMN_PAGES);
  });
});

describe("readOn", () => {
  it("reads on from what is loaded, shows each page as it lands, and says it reached the end", async () => {
    const all = rows(450);
    const first = await store(all, 30).read(undefined);
    const { read, reads } = store(all, 200);
    const seen: number[] = [];
    const { page, ended } = await readOn(read, first, MAX_COLUMN_PAGES, { onPage: (next) => { seen.push(next.items.length); return true; } });
    expect(ended).toBe(true);
    expect(page.items.map((row) => row.id)).toEqual(all.map((row) => row.id));
    expect(page).toMatchObject({ total: 450, shown: 450, has_more: false, next_cursor: null });
    expect(seen).toEqual([230, 430, 450]);
    expect(reads).toEqual(["t0029", "t0229", "t0429"]);
  });

  it("stops at a row budget or when told to, keeping what it read and the cursor to continue from", async () => {
    const all = rows(1000);
    const first = await store(all, 30).read(undefined);
    const { read } = store(all, 200);
    const budgeted = await readOn(read, first, MAX_COLUMN_PAGES, { maxRows: 400 });
    expect(budgeted.ended).toBe(false);
    expect(budgeted.page.items).toHaveLength(430);
    expect(budgeted.page).toMatchObject({ has_more: true, next_cursor: "t0429", shown: 430 });
    let pages = 0;
    const stopped = await readOn(read, first, MAX_COLUMN_PAGES, { onPage: () => ++pages < 2 });
    expect(stopped.page.items).toHaveLength(430);
    expect(stopped.ended).toBe(false);
  });

  it("ends when a store hands back the cursor it was given", async () => {
    let reads = 0;
    const stuck = async (): Promise<Page<Row>> => { reads++; return { items: [{ id: `s${reads}` }], total: 9_999, shown: 1, has_more: true, next_cursor: "same" }; };
    const { ended } = await readOn(stuck, { items: [], total: 9_999, shown: 0, has_more: true, next_cursor: "same" }, MAX_COLUMN_PAGES);
    expect(ended).toBe(false);
    expect(reads).toBe(1);
  });
});

describe("reloadSize", () => {
  it("keeps the column page size while a reload reaches what is loaded, and takes the store page past it", () => {
    expect(reloadSize(0, 30, 200)).toBe(30);
    expect(reloadSize(30 * MAX_RELOAD_PAGES, 30, 200)).toBe(30);
    expect(reloadSize(30 * MAX_RELOAD_PAGES + 1, 30, 200)).toBe(200);
  });

  it("reads a fully loaded 2,000-card column back whole", async () => {
    const all = rows(2000);
    const size = reloadSize(2000, 30, 200);
    const { read } = store(all, size);
    expect((await reloadPaged(read, 2000, size)).items).toHaveLength(2000);
    expect((await reloadPaged(store(all, 30).read, 2000, 30)).items).toHaveLength(30 * MAX_RELOAD_PAGES);
  });
});

describe("samePage", () => {
  const card = (id: string, revision = 1) => ({ id, revision });
  const page = (items: { id: string; revision: number }[], extra: Partial<Page<{ id: string; revision: number }>> = {}) =>
    ({ items, total: items.length, shown: items.length, has_more: false, next_cursor: null, ...extra });

  it("is the same answer when every record, revision, total and cursor match", () => {
    expect(samePage(page([card("a"), card("b")]), page([card("a"), card("b")]))).toBe(true);
    expect(samePage(undefined, undefined)).toBe(true);
  });

  it("is a different answer for any change a card or the column could show", () => {
    const base = page([card("a"), card("b")]);
    expect(samePage(base, page([card("a"), card("b", 2)]))).toBe(false);
    expect(samePage(base, page([card("b"), card("a")]))).toBe(false);
    expect(samePage(base, page([card("a")]))).toBe(false);
    expect(samePage(base, page([card("a"), card("b")], { total: 3 }))).toBe(false);
    expect(samePage(base, page([card("a"), card("b")], { next_cursor: "b", has_more: true }))).toBe(false);
    expect(samePage(base, undefined)).toBe(false);
    expect(sameRecords([card("a")], [card("c")])).toBe(false);
  });

  it("is a different answer when a field changed under the same revision", () => {
    // A workspace's repository_count is derived: membership changes it without
    // the workspace's own revision moving. Neither may keep the stale row.
    const space = (count: number) => ({ id: "w", revision: 3, repository_count: count });
    expect(sameRecords([space(0)], [space(1)])).toBe(false);
    const task = (severity: string) => ({ id: "t", revision: 1, severity });
    expect(samePage(page([task("")]), page([task("High")]))).toBe(false);
    expect(sameRecords([space(2)], [space(2)])).toBe(true);
  });
});
