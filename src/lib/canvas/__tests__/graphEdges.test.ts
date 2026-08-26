import { describe, expect, it } from "vitest";
import type { VisualCommitRow } from "../GraphRenderer";
import {
  buildIncomingEdgeIndex,
  deepestChildTargetingRange,
  isLongConnection,
} from "../graphEdges";

function row(
  id: string,
  connections: Array<{ to_row_offset: number }> = [],
): VisualCommitRow {
  return {
    id,
    parent_ids: [],
    summary: id,
    author_name: "ada",
    author_email: "ada@example.com",
    timestamp: 1,
    lane: 0,
    color_index: 0,
    active_lanes: [0],
    active_lane_colors: [0],
    connections: connections.map((c) => ({
      from_lane: 0,
      to_lane: 0,
      is_merge: false,
      color_index: 0,
      ...c,
    })),
    is_merge: false,
    is_root: false,
  };
}

describe("buildIncomingEdgeIndex", () => {
  it("inverts child-owned connections into per-target child lists", () => {
    // child at row 0 -> parent row 3 (offset 3); child row 1 -> parent 2.
    const rows = [
      row("a", [{ to_row_offset: 3 }]),
      row("b", [{ to_row_offset: 1 }]),
      row("c"),
      row("d"),
    ];
    const index = buildIncomingEdgeIndex(rows);
    const list = (t: number) =>
      Array.from(
        index.children.subarray(index.starts[t], index.starts[t + 1]),
      );
    expect(list(2)).toEqual([1]);
    expect(list(3)).toEqual([0]);
    expect(list(0)).toEqual([]);
  });

  it("memoizes on array identity and rebuilds for a new array", () => {
    const rows = [row("a", [{ to_row_offset: 1 }]), row("b")];
    expect(buildIncomingEdgeIndex(rows)).toBe(buildIncomingEdgeIndex(rows));
    const copy = [...rows];
    expect(buildIncomingEdgeIndex(copy)).not.toBe(buildIncomingEdgeIndex(rows));
  });

  it("ignores hostile offsets: negative, self, and past-the-end targets", () => {
    const rows = [
      row("a", [
        { to_row_offset: -5 },
        { to_row_offset: Number.NaN },
        { to_row_offset: 99 },
        { to_row_offset: 1 },
      ]),
      row("b"),
    ];
    const index = buildIncomingEdgeIndex(rows);
    // Only the offset-1 edge lands inside; NaN must not corrupt the CSR fill
    // by advancing a cursor without a counted slot.
    const list = Array.from(
      index.children.subarray(index.starts[1], index.starts[2]),
    );
    expect(list).toEqual([0]);
  });

  it("preserves duplicate edges from repeated parents", () => {
    const rows = [row("a", [{ to_row_offset: 1 }, { to_row_offset: 1 }]), row("b")];
    const index = buildIncomingEdgeIndex(rows);
    const list = Array.from(
      index.children.subarray(index.starts[1], index.starts[2]),
    );
    expect(list).toEqual([0, 0]);
  });
});

describe("deepestChildTargetingRange", () => {
  it("finds the closest child above the window that reaches into it", () => {
    // A merge whose branch tip sits 500 rows above its parent row.
    const rows: VisualCommitRow[] = [];
    for (let i = 0; i < 600; i++) rows.push(row(`c${i}`));
    rows[100].connections.push({
      from_lane: 0,
      to_lane: 0,
      to_row_offset: 400, // target row 500
      is_merge: true,
      color_index: 0,
    });
    rows[480].connections.push({
      from_lane: 0,
      to_lane: 0,
      to_row_offset: 30, // target row 510 — nearer child
      is_merge: false,
      color_index: 0,
    });

    const index = buildIncomingEdgeIndex(rows);
    expect(deepestChildTargetingRange(index, 495, 520)).toBe(100);
    expect(deepestChildTargetingRange(index, 505, 520)).toBe(480);
    expect(deepestChildTargetingRange(index, 590, 600)).toBe(590);
  });
});

describe("isLongConnection", () => {
  it("owns the strip/overlay boundary at exactly the lookback bound", () => {
    expect(isLongConnection({ to_row_offset: 60 }, 60)).toBe(false);
    expect(isLongConnection({ to_row_offset: 61 }, 60)).toBe(true);
  });
});
