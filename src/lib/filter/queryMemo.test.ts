import { describe, expect, it } from "vitest";
import { parseFilterQuery } from "./parseQuery";
import {
  createCachedQueryParser,
  createRowFilterMemo,
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
  active_lane_colors?: number[];
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

/** A solved graph row: carries parent ids and solver-baked connections. */
interface GraphRow extends Row {
  parent_ids: string[];
  is_merge?: boolean;
  connections: Array<{
    from_lane: number;
    to_lane: number;
    to_row_offset: number;
    is_merge: boolean;
    color_index: number;
    is_dangling?: boolean;
  }>;
}

function conn(
  overrides: Partial<GraphRow["connections"][number]> = {},
): GraphRow["connections"][number] {
  return {
    from_lane: 0,
    to_lane: 0,
    to_row_offset: 1,
    is_merge: false,
    color_index: 0,
    ...overrides,
  };
}

function graphRow(overrides: Partial<GraphRow> & { id: string }): GraphRow {
  return { ...row(overrides), parent_ids: [], connections: [], ...overrides };
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
    // Lane 5 only appeared as a through-column on this row; its occupant
    // was filtered out, so it is dropped rather than renamed. The survivor
    // packs onto column 0.
    expect(result.maxActiveLane).toBe(0);
    expect(result.rows[0].lane).toBe(0);
    expect(result.rows[0].active_lanes).toEqual([0]);
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

  it("remaps to_row_offset onto filtered coordinates instead of pointing at strangers", () => {
    // The solver bakes parent_index - child_index into every connection
    // AGAINST THE ARRAY IT SOLVED. Filtering out the middle row ("gone")
    // used to leave c2's offset at 2 — landing on whatever shifted into
    // that slot — and c1's offset pointing past the end, silently dropped.
    // Distinct authors so a single author: token separates keep from drop
    // (free text is a word-AND, which cannot express "keep two OR root").
    const rows = [
      graphRow({
        id: "c2",
        summary: "keep two",
        author_name: "ada",
        parent_ids: ["c0"],
        connections: [conn({ to_row_offset: 2 })],
      }),
      graphRow({ id: "gone", summary: "drop me", author_name: "zoe", author_email: "zoe@elsewhere.io" }),
      graphRow({ id: "c0", summary: "root", author_name: "ada" }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("author:ada"));
    expect(result.rows.map((r) => r.id)).toEqual(["c2", "c0"]);
    expect(result.rows[0].connections[0].to_row_offset).toBe(1);
    expect(result.rows[0].connections[0].is_dangling).toBeFalsy();
  });

  it("marks an edge dangling when its parent was filtered away, never into a stranger", () => {
    // The honest answer to "parent is not visible" is a stub, not a line
    // drawn into an unrelated commit that happens to occupy the slot.
    const rows = [
      graphRow({
        id: "child",
        summary: "keep",
        parent_ids: ["hidden"],
        connections: [conn({ to_row_offset: 1 })],
      }),
      graphRow({ id: "hidden", summary: "drop me" }),
      graphRow({ id: "bystander", summary: "root keep" }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("keep"));
    expect(result.rows.map((r) => r.id)).toEqual(["child", "bystander"]);
    const edge = result.rows[0].connections[0];
    expect(edge.is_dangling).toBe(true);
    // A dangling stub protrudes one row; it must not reach the bystander.
    expect(edge.to_row_offset).toBe(1);
  });

  it("densifies surviving lanes so a filter cannot leave a wide empty gutter", () => {
    // Solver lanes are baked against the FULL history. Dropping the rows that
    // occupied 1..7 used to leave a survivor on lane 8, so measureWidth
    // still reserved nine columns and connectors ran horizontally across
    // empty space. The visible set only uses two columns — pack them.
    const rows = [
      graphRow({
        id: "tip",
        summary: "keep tip",
        lane: 0,
        active_lanes: [0, 8],
        parent_ids: ["base"],
        connections: [conn({ from_lane: 0, to_lane: 8, to_row_offset: 2 })],
      }),
      graphRow({
        id: "noise",
        summary: "drop me",
        lane: 3,
        active_lanes: [3],
      }),
      graphRow({
        id: "base",
        summary: "keep base",
        lane: 8,
        active_lanes: [8],
      }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("keep"));
    expect(result.rows.map((r) => r.id)).toEqual(["tip", "base"]);
    expect(result.maxActiveLane).toBe(1);
    expect(result.rows[0].lane).toBe(0);
    expect(result.rows[0].connections[0].to_lane).toBe(1);
    expect(result.rows[1].lane).toBe(1);
    expect(result.rows[0].active_lanes).toEqual([0, 1]);
  });

  it("drops through-lanes whose occupant was filtered out, then densifies", () => {
    // active_lanes records every column that passed through the row in the
    // FULL history. After the occupant of lane 3 is gone, leaving it in the
    // array paints a ghost vertical and, after densify-by-rename, a spare
    // column that no surviving commit occupies.
    const rows = [
      graphRow({
        id: "tip",
        summary: "keep tip",
        lane: 0,
        active_lanes: [0, 3, 8],
        active_lane_colors: [10, 11, 12],
        parent_ids: ["base"],
        connections: [conn({ from_lane: 0, to_lane: 8, to_row_offset: 2 })],
      }),
      graphRow({
        id: "ghost-branch",
        summary: "drop me",
        lane: 3,
        active_lanes: [3],
        active_lane_colors: [11],
      }),
      graphRow({
        id: "base",
        summary: "keep base",
        lane: 8,
        active_lanes: [8],
        active_lane_colors: [12],
      }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("keep"));
    expect(result.rows.map((r) => r.id)).toEqual(["tip", "base"]);
    expect(result.maxActiveLane).toBe(1);
    expect(result.rows[0].active_lanes).toEqual([0, 1]);
    expect(result.rows[0].active_lane_colors).toEqual([10, 12]);
    expect(result.rows[0].connections[0].to_lane).toBe(1);
  });

  it("does not keep a dangling parent's column in the gutter", () => {
    // Stubs are drawn on from_lane. A to_lane that only existed for the
    // dropped parent must not survive densify and widen measureWidth.
    const rows = [
      graphRow({
        id: "tip",
        summary: "keep tip",
        lane: 0,
        active_lanes: [0],
        parent_ids: ["gone"],
        connections: [conn({ from_lane: 0, to_lane: 9, to_row_offset: 1 })],
      }),
      graphRow({
        id: "gone",
        summary: "drop me",
        lane: 9,
        active_lanes: [9],
      }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("keep"));
    expect(result.rows).toHaveLength(1);
    expect(result.rows[0].connections[0].is_dangling).toBe(true);
    expect(result.rows[0].connections[0].to_lane).toBe(0);
    expect(result.maxActiveLane).toBe(0);
  });

  it("remaps merge edges by parent identity, then densifies leftover columns", () => {
    // Connection k ↔ parent_ids[k] is the solver's contract; colors stay
    // per-row. Lane *indices* are renamed onto 0..k-1 so dropped columns
    // cannot keep the gutter wide.
    const rows = [
      graphRow({
        id: "m",
        summary: "merge keep",
        is_merge: true,
        parent_ids: ["main", "side"],
        connections: [
          conn({ to_row_offset: 3, from_lane: 0, to_lane: 0, color_index: 0 }),
          conn({ to_row_offset: 2, from_lane: 0, to_lane: 4, is_merge: true, color_index: 7 }),
        ],
      }),
      graphRow({ id: "noise", summary: "drop me" }),
      graphRow({ id: "gap", summary: "also drop" }),
      graphRow({ id: "main", summary: "mainline keep" }),
      graphRow({ id: "side", summary: "branch tip keep" }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("keep"));
    expect(result.rows.map((r) => r.id)).toEqual(["m", "main", "side"]);
    const [firstParent, secondParent] = result.rows[0].connections;
    expect(firstParent.to_row_offset).toBe(1);
    expect(firstParent.from_lane).toBe(0);
    // side sat at original index 4, two rows were removed before it: 4-0=2.
    expect(secondParent.to_row_offset).toBe(2);
    // Distinct columns stay distinct; unused indices (1..3) are squeezed so
    // the original to_lane 4 becomes 1.
    expect(secondParent.to_lane).toBe(1);
    expect(secondParent.to_lane).not.toBe(firstParent.to_lane);
    expect(secondParent.is_merge).toBe(true);
    expect(secondParent.color_index).toBe(7);
  });

  it("returns the input array untouched when nothing was filtered out", () => {
    // Identity preservation keeps downstream canvas caches (keyed on array
    // identity) from re-rasterizing on no-op filters.
    const rows = [
      graphRow({ id: "a", summary: "feat: one", parent_ids: ["b"], connections: [conn()] }),
      graphRow({ id: "b", summary: "root" }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery(""));
    expect(result.rows).toBe(rows as unknown as GraphRow[]);
  });

  it("keeps original row objects when no connection pointed past a removal", () => {
    // Removals BELOW every referenced parent change no offsets; copying rows
    // would churn identity for nothing.
    // Solver-truthful offsets: top(0)→base(2) is 2, mid(1)→base(2) is 1.
    const topConn = conn({ to_row_offset: 2 });
    const midConn = conn({ to_row_offset: 1 });
    const rows = [
      graphRow({ id: "top", summary: "feat: top", parent_ids: ["base"], connections: [topConn] }),
      graphRow({ id: "mid", summary: "feat: mid", parent_ids: ["base"], connections: [midConn] }),
      graphRow({ id: "base", summary: "feat: base" }),
      graphRow({ id: "tail", summary: "chore: drop me" }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("feat"));
    expect(result.rows.map((r) => r.id)).toEqual(["top", "mid", "base"]);
    expect(result.rows[0]).toBe(rows[0]);
    expect(result.rows[1]).toBe(rows[1]);
  });

  it("treats already-dangling edges as untouched by remapping", () => {
    const rows = [
      graphRow({
        id: "tip",
        summary: "feat: tip",
        parent_ids: ["cut-off"],
        connections: [conn({ to_row_offset: 1, is_dangling: true })],
      }),
      graphRow({ id: "dropped", summary: "chore: gone" }),
    ];
    const result = filterRowsWithLanes(rows, parseFilterQuery("feat"));
    expect(result.rows[0].connections[0].is_dangling).toBe(true);
    expect(result.rows[0].connections[0]).toBe(rows[0].connections[0]);
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

describe("createRowFilterMemo", () => {
  it("returns the identical result object for repeated (rows, query) inputs", () => {
    const memo = createRowFilterMemo();
    const rows = [
      row({ id: "a1", summary: "feat: one", lane: 0, active_lanes: [0] }),
      row({ id: "b2", summary: "fix: two", lane: 1, active_lanes: [1] }),
    ];
    const parsed = parseFilterQuery("");

    const first = memo.filter(rows, parsed);
    // The whole point: a store emission that reuses the same rows identity
    // must not hand derivations a fresh array — that identity is what keys
    // the canvas strip cache.
    for (let i = 0; i < 3; i += 1) {
      expect(memo.filter(rows, parsed)).toBe(first);
    }
  });

  it("recomputes when the parsed query differs but keeps per-query results", () => {
    const memo = createRowFilterMemo();
    // Production hands the memo the FROZEN, SHARED parsed object from the
    // parse memo — identity-stable per raw string. Key on that contract.
    const parser = createCachedQueryParser(4);
    const rows = [row({ id: "a1", summary: "feat: one" })];
    const all = memo.filter(rows, parser.parse(""));
    const typed = memo.filter(rows, parser.parse("one"));

    expect(typed).not.toBe(all);
    expect(memo.filter(rows, parser.parse(""))).toBe(all);
    expect(memo.filter(rows, parser.parse("one"))).toBe(typed);
  });

  it("recomputes when the rows identity changes", () => {
    const memo = createRowFilterMemo();
    const parsed = parseFilterQuery("");
    const first = memo.filter([row({ id: "a1" })], parsed);
    const second = memo.filter([row({ id: "a1" })], parsed);
    expect(second).not.toBe(first);
    expect(second.rows).toHaveLength(1);
  });

  it("caps the per-rows query map without corrupting results", () => {
    const memo = createRowFilterMemo();
    const parser = createCachedQueryParser(64);
    const rows = [row({ id: "a1", summary: "alpha beta" })];
    let first: ReturnType<typeof memo.filter> | null = null;
    for (let i = 0; i < 40; i += 1) {
      const result = memo.filter(rows, parser.parse(`needle-${i}`));
      if (i === 0) first = result;
    }
    // The oldest entry may have been evicted; recomputing it must still be
    // correct even if the reference differs from the very first result.
    const refetched = memo.filter(rows, parser.parse("needle-0"));
    expect(refetched.rows).toEqual(first?.rows ?? null);
    expect(refetched.maxActiveLane).toBe(first?.maxActiveLane ?? 0);
  });
});
