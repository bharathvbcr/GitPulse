import { describe, expect, it } from "vitest";
import { parseFilterQuery } from "./parseQuery";
import {
  createCachedQueryParser,
  filterRowsWithLanes,
  PARSE_MEMO_CAP,
  parseFilterQueryCached,
  queryParser,
} from "./queryMemo";

describe("query memo cache poisoning", () => {
  it("hands out frozen objects whose fields can never drift", () => {
    const raw = "author:ada fix gate";
    const parsed = parseFilterQueryCached(raw);
    // Documented contract (queryMemo.ts): returned objects are frozen.
    expect(Object.isFrozen(parsed)).toBe(true);

    let mutationError: unknown = null;
    try {
      (parsed as { text: string }).text = "poisoned";
      (parsed as { author?: string }).author = "mallory";
    } catch (error) {
      mutationError = error;
    }
    // Strict-mode ESM throws TypeError on a frozen write; if a future host
    // silently ignores it instead, the assertions below still hold.
    if (mutationError !== null) {
      expect(mutationError).toBeInstanceOf(TypeError);
    }

    // The same object comes back and its fields are pristine either way.
    expect(parseFilterQueryCached(raw)).toBe(parsed);
    expect(parsed.text).toBe("fix gate");
    expect(parsed.author).toBe("ada");
  });
});

describe("query memo cap=1 thrash", () => {
  it("stays correct when A,B alternate forever", () => {
    const parser = createCachedQueryParser(1);
    const queries = ["author:a alpha", "author:b beta"];
    for (let round = 0; round < 40; round += 1) {
      for (const raw of queries) {
        const out = parser.parse(raw);
        expect(out).toEqual(parseFilterQuery(raw));
        expect(parser.size).toBeLessThanOrEqual(1);
      }
    }
    // Cap 1 means every parse after the very first evicts: 80 parses,
    // 80 misses.
    expect(parser.misses).toBe(80);
  });
});

describe("parseFilterQueryCached module singleton under load", () => {
  it("survives 10k distinct queries under 2s with the cache capped", () => {
    const startedAt = performance.now();
    for (let i = 0; i < 10_000; i += 1) {
      const out = parseFilterQueryCached(`needle-${i} type:fix`);
      expect(out.text).toBe(`needle-${i}`);
      expect(queryParser.size).toBeLessThanOrEqual(PARSE_MEMO_CAP);
    }
    const elapsedMs = performance.now() - startedAt;
    // Tripwire against pathological blowups, not an SLA: this suite
    // shares the machine with Rust builds.
    expect(elapsedMs).toBeLessThan(15_000);
    expect(queryParser.size).toBe(PARSE_MEMO_CAP);

    // Correctness spot check through the shared instance afterwards.
    expect(parseFilterQueryCached("author:grace hopper")).toEqual(
      parseFilterQuery("author:grace hopper")
    );
  });
});

interface Row {
  id: string;
  summary: string;
  author_name: string;
  author_email: string;
  lane?: number;
  active_lanes?: number[];
}

function row(overrides: Partial<Row> & { id: string }): Row {
  return {
    summary: "init",
    author_name: "ada",
    author_email: "ada@example.com",
    ...overrides,
  };
}

describe("filterRowsWithLanes stress: adversarial lane data", () => {
  it("keeps NaN-only lanes from widening the result while rows survive", () => {
    const rows = [
      row({ id: "n1", lane: Number.NaN, active_lanes: [Number.NaN] }),
      row({ id: "n2", active_lanes: [Number.NaN, Number.NaN] }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery(""));
    expect(result.rows.map((r) => r.id)).toEqual(["n1", "n2"]);
    expect(result.maxActiveLane).toBe(0);
  });

  it("collapses duplicate lane values without double counting", () => {
    const rows = [row({ id: "d", lane: 5, active_lanes: [5, 5, 5, 3, 3] })];
    expect(filterRowsWithLanes(rows, parseFilterQuery(""))).toEqual({
      rows: [rows[0]],
      maxActiveLane: 5,
    });
  });

  it("tolerates duplicate row identities", () => {
    const rows = [
      row({ id: "same", summary: "dup", lane: 2, active_lanes: [] }),
      row({ id: "same", summary: "dup", lane: 2, active_lanes: [] }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("dup"));
    expect(result.rows).toHaveLength(2);
    expect(result.maxActiveLane).toBe(2);
  });
});
