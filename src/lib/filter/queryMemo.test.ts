import { describe, expect, it } from "vitest";
import { parseFilterQuery } from "./parseQuery";
import {
  createCachedQueryParser,
  filterRowsWithLanes,
  PARSE_MEMO_CAP,
} from "./queryMemo";

describe("createCachedQueryParser", () => {
  it("parses each distinct raw string once and serves repeats from the memo", () => {
    const parser = createCachedQueryParser(4);
    expect(parser.size).toBe(0);

    const first = parser.parse("author:ada fix: gate");
    expect(parser.misses).toBe(1);
    expect(parser.size).toBe(1);

    for (let i = 0; i < 5; i += 1) {
      expect(parser.parse("author:ada fix: gate")).toBe(first);
    }
    expect(parser.misses).toBe(1);
    expect(first).toEqual(parseFilterQuery("author:ada fix: gate"));
  });

  it("evicts the least recently used entry beyond the cap", () => {
    const parser = createCachedQueryParser(2);
    parser.parse("one");
    parser.parse("two");
    // Refresh "one" so "two" becomes the LRU entry.
    expect(parser.parse("one")).toEqual(parseFilterQuery("one"));
    parser.parse("three");
    expect(parser.size).toBe(2);
    expect(parser.parse("three")).toEqual(parseFilterQuery("three")); // still cached
    expect(parser.misses).toBe(3); // one, two, three — never a fourth

    // "two" was evicted; asking for it is a fresh miss while size stays capped.
    parser.parse("two");
    expect(parser.size).toBe(2);
    expect(parser.misses).toBe(4);
  });

  it("caps at the documented default size", () => {
    const parser = createCachedQueryParser(PARSE_MEMO_CAP);
    for (let i = 0; i < PARSE_MEMO_CAP + 10; i += 1) {
      parser.parse(`query-${i}`);
    }
    expect(parser.size).toBe(PARSE_MEMO_CAP);
  });
});

interface Row {
  id: string;
  summary: string;
  author_name: string;
  author_email: string;
  lane: number;
  active_lanes: number[];
}

function row(overrides: Partial<Row> & { id: string }): Row {
  return {
    summary: "init",
    author_name: "ada",
    author_email: "ada@example.com",
    lane: 0,
    active_lanes: [],
    ...overrides,
  };
}

describe("filterRowsWithLanes", () => {
  it("filters and finds maxActiveLane in one pass", () => {
    const rows = [
      row({ id: "a1", summary: "feat: one", lane: 0, active_lanes: [0, 1] }),
      row({ id: "b2", summary: "fix: two", lane: 3, active_lanes: [3, 5] }),
      row({ id: "c3", summary: "chore: three", lane: 2, active_lanes: [2] }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("type:fix"));
    expect(result.rows.map((r) => r.id)).toEqual(["b2"]);
    expect(result.maxActiveLane).toBe(5);
  });

  it("reports zero lanes for an empty or fully-filtered set", () => {
    expect(filterRowsWithLanes([], parseFilterQuery(""))).toEqual({
      rows: [],
      maxActiveLane: 0,
    });
    const rows = [row({ id: "x", summary: "wip", lane: 9, active_lanes: [9] })];
    expect(filterRowsWithLanes(rows, parseFilterQuery("nomatch-here")).maxActiveLane).toBe(0);
  });

  it("ignores NaN and missing lane data instead of poisoning the max", () => {
    const rows = [
      row({ id: "n1", lane: Number.NaN, active_lanes: [Number.NaN, 4] }),
      row({ id: "n2", lane: 1, active_lanes: [] as number[] }),
    ];
    expect(filterRowsWithLanes(rows, parseFilterQuery("")).maxActiveLane).toBe(4);
  });

  it("never lets a filtered-out row widen the lane result", () => {
    const rows = [
      row({ id: "keep", lane: 0, active_lanes: [0] }),
      row({ id: "drop", summary: "unrelated", lane: 99, active_lanes: [99] }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("keep"));
    expect(result.rows.map((r) => r.id)).toEqual(["keep"]);
    expect(result.maxActiveLane).toBe(0);
  });

  it("short-circuits on the first matching field without touching later ones", () => {
    // An author miss must not consult sha/type/text: matchesCommit's field
    // dispatch exits early, so a row failing author can't pass via text.
    const rows = [
      row({ id: "ffff", summary: "feat: needle", author_name: "grace" }),
    ];
    expect(filterRowsWithLanes(rows, parseFilterQuery("author:nobody needle"))).toEqual({
      rows: [],
      maxActiveLane: 0,
    });
    expect(filterRowsWithLanes(rows, parseFilterQuery("author:grace needle")).rows).toHaveLength(1);
  });
});
