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
/** `17.0` < `18.0`, compared as numbers per dotted part. */
function compareUnicode(a: string, b: string): number {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < Math.max(pa.length, pb.length); i += 1) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

const committedTable = JSON.parse(readFileSync(TABLE_PATH, "utf8")) as { unicode: string };
const runningUnicode = String(process.versions.unicode);
/**
 * A Node older than the table cannot reproduce it: its regex classes predate
 * the table's code points. That is not drift, so the comparison is skipped,
 * and reported as skipped (never as passed). CI pins an older Node than the
 * machines that regenerate the table, which is how this test came to be red
 * everywhere at once.
 */
const nodeIsOlder = compareUnicode(runningUnicode, committedTable.unicode) < 0;

describe("hidden code point table", () => {
  it("compares Unicode versions numerically", () => {
    expect(compareUnicode("17.0", "18.0")).toBeLessThan(0);
    expect(compareUnicode("18.0", "17.0")).toBeGreaterThan(0);
    expect(compareUnicode("16.0", "16.0")).toBe(0);
    expect(compareUnicode("9.0", "10.0")).toBeLessThan(0);
  });

  it.skipIf(nodeIsOlder)(
    `is what this Node's Unicode data generates (skipped on a Node older than table's Unicode ${committedTable.unicode}; this Node has ${runningUnicode})`,
    () => {
      // Same version: the table must be exactly what it generates. Newer: the
      // table is stale, and regenerating is the fix — a Unicode update still
      // fails loudly on the first machine that has it.
      const fresh = generate() as { unicode: string };
      expect(
        committedTable,
        `committed table is Unicode ${committedTable.unicode}, this Node has ${fresh.unicode}: run node scripts/hidden-codepoints.mjs and commit the result`,
      ).toEqual(fresh);
    },
  );

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
