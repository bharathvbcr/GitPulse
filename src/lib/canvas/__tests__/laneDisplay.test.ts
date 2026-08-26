import { describe, expect, it } from "vitest";
import type { VisualCommitRow } from "../GraphRenderer";
import { collectLiveLanes, liveLanesByRow, maxOccupiedLane } from "../laneDisplay";

/**
 * Lanes are stable columns: this module reports occupancy, it never renames
 * lanes. The old per-row packing (`displayColumn`) is deliberately gone —
 * it made every lane right of a dying neighbour jog sideways and let
 * hit-testing drift off the drawn pixels.
 */

function row(
  overrides: Partial<VisualCommitRow> & { id: string },
): VisualCommitRow {
  return {
    parent_ids: [],
    summary: "s",
    author_name: "Dev",
    author_email: "dev@example.com",
    timestamp: 1,
    lane: 0,
    color_index: 0,
    active_lanes: [0],
    active_lane_colors: [0],
    connections: [],
    is_merge: false,
    is_root: false,
    ...overrides,
  };
}

describe("collectLiveLanes", () => {
  it("reports live lanes at their own indices, holes preserved", () => {
    const live = collectLiveLanes(row({ id: "a", lane: 8, active_lanes: [0, 8] }));
    expect(live).toEqual([0, 8]);
  });

  it("ignores dangling to_lane so a stub cannot keep a ghost column", () => {
    const live = collectLiveLanes(
      row({
        id: "tip",
        lane: 0,
        active_lanes: [0],
        connections: [
          {
            from_lane: 0,
            to_lane: 9,
            to_row_offset: 1,
            is_merge: false,
            color_index: 0,
            is_dangling: true,
          },
        ],
      }),
    );
    expect(live).toEqual([0]);
  });

  it("keeps a child from_lane that active_lanes omitted", () => {
    const live = collectLiveLanes(
      row({
        id: "child",
        lane: 0,
        active_lanes: [0],
        connections: [
          {
            from_lane: 5,
            to_lane: 0,
            to_row_offset: 1,
            is_merge: false,
            color_index: 1,
          },
        ],
      }),
    );
    expect(live).toEqual([0, 5]);
  });

  it("does not treat a destination lane as occupancy on the child row", () => {
    // The target column belongs to the parent's segment; on the child's own
    // row it is the connector's business, not this row's occupancy.
    const live = collectLiveLanes(
      row({
        id: "child",
        lane: 0,
        active_lanes: [0],
        connections: [
          {
            from_lane: 0,
            to_lane: 5,
            to_row_offset: 1,
            is_merge: true,
            color_index: 1,
          },
        ],
      }),
    );
    expect(live).toEqual([0]);
  });

  it("drops NaN and negative lanes from hostile payloads", () => {
    const live = collectLiveLanes(
      row({
        id: "hostile",
        lane: Number.NaN,
        active_lanes: [Number.NaN, -3, 2],
      }),
    );
    expect(live).toEqual([2]);
  });
});

describe("maxOccupiedLane", () => {
  it("returns the highest occupied column, holes included", () => {
    const gapped = [
      row({ id: "a", lane: 0, active_lanes: [0, 8] }),
      row({ id: "b", lane: 8, active_lanes: [8] }),
    ];
    const dense = [
      row({ id: "a", lane: 0, active_lanes: [0, 1] }),
      row({ id: "b", lane: 1, active_lanes: [1] }),
    ];
    // A branch on column 8 needs the gutter to reach column 8 — collapsing
    // the hole is exactly the repacking this module no longer does.
    expect(maxOccupiedLane(gapped)).toBe(8);
    expect(maxOccupiedLane(dense)).toBe(1);
  });

  it("counts a live destination column but never a dangling one", () => {
    const liveHop = [
      row({
        id: "child",
        lane: 0,
        connections: [
          { from_lane: 0, to_lane: 6, to_row_offset: 1, is_merge: true, color_index: 1 },
        ],
      }),
    ];
    const danglingHop = [
      row({
        id: "tip",
        lane: 0,
        connections: [
          {
            from_lane: 0,
            to_lane: 20,
            to_row_offset: 1,
            is_merge: false,
            color_index: 0,
            is_dangling: true,
          },
        ],
      }),
    ];
    expect(maxOccupiedLane(liveHop)).toBe(6);
    expect(maxOccupiedLane(danglingHop)).toBe(0);
  });

  it("stays zero for empty or NaN-only rows", () => {
    expect(maxOccupiedLane([])).toBe(0);
    expect(
      maxOccupiedLane([
        row({ id: "n", lane: Number.NaN, active_lanes: [Number.NaN] }),
      ]),
    ).toBe(0);
  });
});

describe("liveLanesByRow", () => {
  it("reports each row's occupancy and memoizes on array identity", () => {
    const rows = [
      row({ id: "a", lane: 0, active_lanes: [0, 8] }),
      row({ id: "b", lane: 8, active_lanes: [8] }),
    ];
    const first = liveLanesByRow(rows);
    expect(first[0]).toEqual([0, 8]);
    expect(first[1]).toEqual([8]);
    expect(liveLanesByRow(rows)).toBe(first);
  });
});
