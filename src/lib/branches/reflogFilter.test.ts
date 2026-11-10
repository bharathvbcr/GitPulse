import { describe, expect, it } from "vitest";
import type { ReflogEntry } from "./types";
import { filterReflogEntries } from "./reflogFilter";

function entry(index: number, commit_id: string, action: string, message: string): ReflogEntry {
  return { index, commit_id, selector: `HEAD@{${index}}`, action, message, timestamp: 0 };
}

const ENTRIES = [
  entry(0, "ff072cf2156dd647d5c28507d285e12981097344", "reset", "moving to HEAD~1"),
  entry(1, "8236ad3c8eb367998570ef43bbcd4164381e8ce9", "commit", "Fix the parser"),
  entry(2, "ff072cf2156dd647d5c28507d285e12981097344", "commit (initial)", "one"),
];

describe("filterReflogEntries", () => {
  it("keeps every entry, as the same array, for an empty or blank query", () => {
    expect(filterReflogEntries(ENTRIES, "")).toBe(ENTRIES);
    expect(filterReflogEntries(ENTRIES, "   ")).toBe(ENTRIES);
  });

  it("matches the message and action case-insensitively", () => {
    expect(filterReflogEntries(ENTRIES, "PARSER").map((e) => e.index)).toEqual([1]);
    expect(filterReflogEntries(ENTRIES, "reset").map((e) => e.index)).toEqual([0]);
  });

  it("matches a short sha prefix and the selector", () => {
    expect(filterReflogEntries(ENTRIES, "8236ad3").map((e) => e.index)).toEqual([1]);
    expect(filterReflogEntries(ENTRIES, "HEAD@{2}").map((e) => e.index)).toEqual([2]);
  });

  it("requires every term to match", () => {
    expect(filterReflogEntries(ENTRIES, "commit one").map((e) => e.index)).toEqual([2]);
    expect(filterReflogEntries(ENTRIES, "commit nothing-like-this")).toEqual([]);
  });
});
