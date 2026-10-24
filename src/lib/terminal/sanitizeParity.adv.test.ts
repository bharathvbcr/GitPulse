import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { clean } from "./sessionActivity";

const here = dirname(fileURLToPath(import.meta.url));
type Table = { unicode: string; drop: Array<[number, number]>; space: Array<[number, number]> };
const table = JSON.parse(readFileSync(join(here, "../../../scripts/fixtures/hidden-codepoints.json"), "utf8")) as Table;

function inRanges(ranges: Array<[number, number]>, cp: number): boolean {
  return ranges.some(([lo, hi]) => cp >= lo && cp <= hi);
}

describe("clean() over every code point (fixture shared with Rust alerts::text)", () => {
  it("drops exactly Cf + Co, spaces exactly Cc + whitespace, keeps the rest", () => {
    const bad: string[] = [];
    for (let cp = 0; cp <= 0x10ffff; cp++) {
      if (cp >= 0xd800 && cp <= 0xdfff) continue;
      const out = clean(`a${String.fromCodePoint(cp)}b`, 240);
      const kind = out === "ab" ? "drop" : out === "a b" ? "space" : out === `a${String.fromCodePoint(cp)}b` ? "keep" : "other";
      const want = inRanges(table.drop, cp) ? "drop" : inRanges(table.space, cp) ? "space" : "keep";
      if (kind !== want) bad.push(`U+${cp.toString(16)} ${kind} want ${want}`);
    }
    expect(bad.slice(0, 20), `${bad.length} mismatches`).toEqual([]);
  });

  it("keeps combining marks, variation selectors, RTL and CJK letters", () => {
    for (const s of ["é", "️", "︀", "שלום", "مرحبا", "日本語", "한국어", "क्षि"]) {
      expect(clean(`x${s}y`, 240)).toBe(`x${s}y`);
    }
  });
});
