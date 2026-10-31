import { describe, expect, it } from "vitest";
import { mergeLoadedPage } from "./pageMerge";

interface Row { id: string; rev: number; state?: string }
const row = (id: string, rev = 1, state?: string): Row => ({ id, rev, state });
const ids = (rows: Row[]) => rows.map((item) => item.id);

describe("mergeLoadedPage", () => {
  it("a head refresh keeps the tail the reader paged in", () => {
    const loaded = [row("a"), row("b"), row("c"), row("d"), row("e")];
    const merged = mergeLoadedPage(loaded, [row("a"), row("b")], { more: false });
    expect(ids(merged)).toEqual(["a", "b", "c", "d", "e"]);
  });

  it("a head refresh puts new rows first and lets the fresh row win on an id collision", () => {
    const loaded = [row("b", 1), row("c", 1)];
    const merged = mergeLoadedPage(loaded, [row("n", 1), row("b", 2)], { more: false });
    expect(ids(merged)).toEqual(["n", "b", "c"]);
    expect(merged.find((item) => item.id === "b")?.rev).toBe(2);
  });

  it("a Load more page appends after the loaded rows and still lets the fresh row win", () => {
    const loaded = [row("a", 1), row("b", 1)];
    const merged = mergeLoadedPage(loaded, [row("b", 2), row("c", 1)], { more: true });
    expect(ids(merged)).toEqual(["a", "b", "c"]);
    expect(merged.find((item) => item.id === "b")?.rev).toBe(2);
  });

  it("dropStale removes only the loaded rows a head refresh no longer contains", () => {
    const loaded = [row("p1", 1, "pending"), row("p2", 1, "pending"), row("d1", 1, "decided")];
    const merged = mergeLoadedPage(loaded, [row("p1", 2, "pending")], { more: false, dropStale: (item) => item.state === "pending" });
    expect(ids(merged)).toEqual(["p1", "d1"]);
  });

  it("dropStale never touches a fresh row, and is ignored for a Load more page", () => {
    const loaded = [row("p1", 1, "pending")];
    expect(ids(mergeLoadedPage(loaded, [row("p1", 2, "pending")], { more: false, dropStale: () => true }))).toEqual(["p1"]);
    expect(ids(mergeLoadedPage(loaded, [row("x")], { more: true, dropStale: () => true }))).toEqual(["p1", "x"]);
  });

  it("handles empty inputs without inventing or losing rows", () => {
    expect(mergeLoadedPage<Row>([], [], { more: false })).toEqual([]);
    expect(mergeLoadedPage<Row>([], [], { more: true })).toEqual([]);
    expect(ids(mergeLoadedPage([row("a")], [], { more: false }))).toEqual(["a"]);
    expect(ids(mergeLoadedPage([], [row("a")], { more: false }))).toEqual(["a"]);
  });

  it("does not mutate its inputs", () => {
    const loaded = [row("a"), row("b")];
    const fresh = [row("c")];
    mergeLoadedPage(loaded, fresh, { more: false });
    expect(ids(loaded)).toEqual(["a", "b"]);
    expect(ids(fresh)).toEqual(["c"]);
  });
});
