import { describe, expect, it } from "vitest";
import { createSessionBook } from "./sessionBook";

const same = (a: string, b: string) => a.toLowerCase().replace(/\/$/, "") === b.toLowerCase().replace(/\/$/, "");

describe("sessionBook", () => {
  it("forgets every per-tab and per-path record of a closed tab, and only that tab's", () => {
    const book = createSessionBook(same);
    for (const [id, path] of [["a", "/r/a"], ["b", "/r/b"]] as const) {
      book.familyUnknown.add(id); book.snapshotRuns.set(id, 3); book.pollRuns.set(id, 4);
      book.statsInflight.add(id); book.mutationEchoUntil.set(path, 99);
    }
    expect(book.size()).toBe(10);
    book.forget("a", "/R/A/");
    expect(book.size()).toBe(5);
    expect([...book.snapshotRuns.keys()]).toEqual(["b"]);
    expect([...book.mutationEchoUntil.keys()]).toEqual(["/r/b"]);
  });
  it("forgets the id's records when the path is unknown", () => {
    const book = createSessionBook(same);
    book.snapshotRuns.set("a", 1); book.mutationEchoUntil.set("/r/a", 1);
    book.forget("a", null);
    expect(book.snapshotRuns.size).toBe(0);
    expect(book.mutationEchoUntil.size).toBe(1);
  });
  it("forgetting something never held is a no-op", () => {
    const book = createSessionBook(same);
    book.forget("ghost", "/r/ghost");
    expect(book.size()).toBe(0);
  });
});
