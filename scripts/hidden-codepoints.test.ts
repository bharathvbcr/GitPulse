import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { TABLE_PATH, generate } from "./hidden-codepoints.mjs";

/**
 * The one table of hidden code points (`fixtures/hidden-codepoints.json`) is
 * what Rust's `alerts::text` and the renderer's `clean` both read. It is
 * generated, never edited; this compares it with the `\p{Cf}`, `\p{Co}`,
 * `\p{Cc}` and `\s` classes of the Node running the tests, so a Unicode update
 * fails here and is fixed by `node scripts/hidden-codepoints.mjs`.
 */
describe("hidden code point table", () => {
  it("is what this Node's Unicode data generates", () => {
    const committed = JSON.parse(readFileSync(TABLE_PATH, "utf8")) as { unicode: string };
    const fresh = generate() as { unicode: string };
    expect(
      committed,
      `committed table is Unicode ${committed.unicode}, this Node has ${fresh.unicode}: run node scripts/hidden-codepoints.mjs and commit the result`,
    ).toEqual(fresh);
  });

  it("is sorted, disjoint and covers the classes the notifier relies on", () => {
    const table = JSON.parse(readFileSync(TABLE_PATH, "utf8")) as { drop: Array<[number, number]>; space: Array<[number, number]> };
    for (const ranges of [table.drop, table.space]) {
      ranges.forEach(([lo, hi], i) => {
        expect(lo).toBeLessThanOrEqual(hi);
        if (i > 0) expect(lo).toBeGreaterThan(ranges[i - 1][1] + 1);
      });
    }
    const has = (ranges: Array<[number, number]>, cp: number) => ranges.some(([lo, hi]) => cp >= lo && cp <= hi);
    for (const cp of [0x202e, 0x2066, 0x2069, 0x200b, 0x200f, 0xfeff, 0xe0001, 0xe0041, 0xe000]) expect(has(table.drop, cp), cp.toString(16)).toBe(true);
    for (const cp of [0x0a, 0x09, 0x85, 0x2028, 0x2029, 0x20]) expect(has(table.space, cp), cp.toString(16)).toBe(true);
  });
});
