import { describe, expect, it } from "vitest";
import {
  dedupePreserveOrder,
  eachKeysAreUnique,
  keyedList,
  uniqueKeyAllocator,
} from "./eachKeys";

describe("uniqueKeyAllocator", () => {
  it.each([
    ["a", "a", "a#1"],
    ["a#1", "a", "a"],
    ["", "__empty", "", "__empty#1"],
    ["a", "a", "a#1", "a#1", "a#1#1"],
  ])("keeps literal suffixes and generated keys disjoint: %j", (...bases) => {
    const key = uniqueKeyAllocator();
    const keys = bases.map(key);
    expect(new Set(keys).size).toBe(bases.length);
  });

  it("survives 100,000 adversarial claims without losing a row", () => {
    const bases = Array.from({ length: 100_000 }, (_, i) =>
      i % 3 === 0 ? "" : i % 3 === 1 ? "__empty" : `__empty#${Math.floor(i / 3)}`,
    );
    const rows = keyedList(bases, (base) => base);
    expect(new Set(rows.map((row) => row.key)).size).toBe(bases.length);
    expect(rows.map((row) => row.item)).toEqual(bases);
    expect(keyedList(bases, (base) => base)).toEqual(rows);
  });

  it("leaves the first claimant alone and suffixes collisions", () => {
    const key = uniqueKeyAllocator();
    expect(key("a")).toBe("a");
    expect(key("a")).toBe("a#1");
    expect(key("a")).toBe("a#2");
    expect(key("b")).toBe("b");
  });

  it("does not collapse empty bases onto one anonymous key forever", () => {
    const key = uniqueKeyAllocator();
    expect(key("")).toBe("__empty");
    expect(key("")).toBe("__empty#1");
  });
});

describe("dedupePreserveOrder", () => {
  it("keeps first-seen order and drops later duplicates", () => {
    expect(dedupePreserveOrder(["a", "b", "a", "c", "b"])).toEqual(["a", "b", "c"]);
  });
});

describe("keyedList", () => {
  it("makes Svelte-safe keys when the source repeats an id", () => {
    const rows = keyedList(
      ["x.rs::f", "y.rs::g", "x.rs::f"],
      (id) => id,
    );
    expect(rows.map((r) => r.key)).toEqual(["x.rs::f", "y.rs::g", "x.rs::f#1"]);
    expect(eachKeysAreUnique(rows, (r) => r.key)).toBe(true);
  });

  it("characterizes the pre-fix crash shape: raw ids are not unique", () => {
    const raw = ["a::f", "a::f", "b::g"];
    expect(eachKeysAreUnique(raw, (id) => id)).toBe(false);
  });
});
