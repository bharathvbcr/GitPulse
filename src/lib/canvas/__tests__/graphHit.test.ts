import { describe, expect, it } from "vitest";
import { GraphRenderer, type VisualCommitRow } from "../GraphRenderer";

/**
 * Connector-hover attribution contract.
 *
 * DOCUMENTED GAP (pre-fix): the rows a closing connector descends through —
 * between a branch's last commit and its merge point — carry no occupancy
 * entry (`active_lanes` lists only nodes and pending reservations), so the
 * hit test reported "nothing here" for a line that is plainly drawn on
 * screen. The legacy system and most Git clients share this behaviour; this
 * suite retires it: every visible in-flight closing connector attributes to
 * the CHILD commit that owns the descent (the branch's last commit), with
 * the merge point exposed as the hit's target.
 */

function baseRow(i: number, partial: Partial<VisualCommitRow>): VisualCommitRow {
  return {
    id: `c${i}`,
    parent_ids: [],
    summary: `c${i}`,
    author_name: "ada",
    author_email: "ada@example.com",
    timestamp: 1,
    lane: 0,
    color_index: 0,
    active_lanes: [0],
    active_lane_colors: [0],
    connections: [],
    is_merge: false,
    is_root: false,
    ...partial,
  };
}

/**
 * main (lane 0) with a branch `closer` on lane 1 whose last commit sits at
 * row 1 and merges into main's commit at row `target`. Rows in between show
 * only lane 0 occupancy — lane 1 holds nothing but the descending line.
 */
function closingHistory(target: number): VisualCommitRow[] {
  const rows: VisualCommitRow[] = [];
  rows.push(
    baseRow(0, {
      lane: 0,
      active_lanes: [0, 1],
      active_lane_colors: [0, 1],
      parent_ids: ["c2"],
      connections: [
        { from_lane: 0, to_lane: 0, to_row_offset: 2, is_merge: false, color_index: 0 },
      ],
    }),
  );
  rows.push(
    baseRow(1, {
      id: "closer",
      lane: 1,
      color_index: 1,
      active_lanes: [0, 1],
      active_lane_colors: [0, 1],
      parent_ids: [`c${target}`],
      connections: [
        {
          from_lane: 1,
          to_lane: 0,
          to_row_offset: target - 1,
          is_merge: false,
          color_index: 1,
        },
      ],
    }),
  );
  for (let i = 2; i < target; i++) {
    rows.push(
      baseRow(i, {
        lane: 0,
        parent_ids: [`c${i + 1}`],
        connections: [
          { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 },
        ],
      }),
    );
  }
  rows.push(baseRow(target, { lane: 0, is_root: true }));
  return rows;
}

function probeY(renderer: GraphRenderer, row: number): number {
  const { rowHeight } = renderer.getConfig();
  return row * rowHeight + rowHeight / 2;
}

describe("in-flight closing connectors are hoverable", () => {
  it("attributes the descent to the branch's last commit on every row it crosses", () => {
    const renderer = new GraphRenderer();
    const rows = closingHistory(8);
    const laneX = renderer.getLaneX(1);
    for (const probe of [2, 4, 6, 7]) {
      const hit = renderer.getCommitAtPoint(laneX, probeY(renderer, probe), rows, 0, rows.length);
      expect(
        hit?.id,
        `row ${probe}: the drawn descent must attribute to its owning commit`,
      ).toBe("closer");
    }
  });

  it("exposes the merge point through the typed hit", () => {
    const renderer = new GraphRenderer();
    const rows = closingHistory(8);
    const hit = renderer.getGraphHitAtPoint(
      renderer.getLaneX(1),
      probeY(renderer, 4),
      rows,
      0,
      rows.length,
    );
    expect(hit?.kind).toBe("connector");
    expect(hit?.row.id).toBe("closer");
    expect(hit?.connectorTarget?.id).toBe("c8");
  });

  it("keeps node and pass-through hits owning their own rows", () => {
    const renderer = new GraphRenderer();
    const rows = closingHistory(8);
    // The child row itself is a node hit, not a connector hit.
    const child = renderer.getGraphHitAtPoint(
      renderer.getLaneX(1),
      probeY(renderer, 1),
      rows,
      0,
      rows.length,
    );
    expect(child?.kind).toBe("node");
    expect(child?.row.id).toBe("closer");
    // The merge-point row hits main's node on lane 0.
    const target = renderer.getGraphHitAtPoint(
      renderer.getLaneX(0),
      probeY(renderer, 8),
      rows,
      0,
      rows.length,
    );
    expect(target?.kind).toBe("node");
    expect(target?.row.id).toBe("c8");
    // A live pass-through (lane 0 at the closer's row) resolves through
    // occupancy, not the connector index.
    const lane = renderer.getGraphHitAtPoint(
      renderer.getLaneX(0),
      probeY(renderer, 1),
      rows,
      0,
      rows.length,
    );
    expect(lane?.kind).toBe("lane");
    expect(lane?.connectorTarget).toBeNull();
  });

  it("covers spans longer than the strip-cache lookback", () => {
    const renderer = new GraphRenderer();
    const rows = closingHistory(200);
    const hit = renderer.getGraphHitAtPoint(
      renderer.getLaneX(1),
      probeY(renderer, 130),
      rows,
      0,
      rows.length,
    );
    expect(hit?.kind).toBe("connector");
    expect(hit?.row.id).toBe("closer");
  });

  it("never creates connector hits from dangling or merge-peel edges", () => {
    const renderer = new GraphRenderer();
    // Dangling stub on lane 1 and a merge peel to lane 2: neither owns a
    // closing descent, so the empty columns beside them stay inert.
    const rows: VisualCommitRow[] = [
      baseRow(0, {
        lane: 1,
        is_merge: true,
        parent_ids: ["ghost", "p"],
        connections: [
          {
            from_lane: 1,
            to_lane: 1,
            to_row_offset: 1,
            is_merge: false,
            color_index: 1,
            is_dangling: true,
          },
          { from_lane: 1, to_lane: 2, to_row_offset: 3, is_merge: true, color_index: 2 },
        ],
      }),
      baseRow(1, { lane: 0, active_lanes: [0, 2], active_lane_colors: [0, 2] }),
      baseRow(2, { lane: 0, active_lanes: [0, 2], active_lane_colors: [0, 2] }),
      baseRow(3, { id: "p", lane: 2, active_lanes: [0, 2], active_lane_colors: [0, 2], is_root: true }),
    ];
    const stub = renderer.getGraphHitAtPoint(
      renderer.getLaneX(1),
      probeY(renderer, 2),
      rows,
      0,
      rows.length,
    );
    expect(stub, "a dangling edge must not fabricate a connector hit").toBeNull();
    // The merge peel's column IS occupied (reservation) — it hits as a lane.
    const peel = renderer.getGraphHitAtPoint(
      renderer.getLaneX(2),
      probeY(renderer, 2),
      rows,
      0,
      rows.length,
    );
    expect(peel?.kind).toBe("lane");
    expect(peel?.row.id).toBe("p");
  });

  it("lets an occupant win over a hostile overlapping connector claim", () => {
    const renderer = new GraphRenderer();
    // Corrupt payload: a node parked mid-span on the connector's own lane.
    // Occupancy is truth; the connector must not shadow the node.
    const rows = closingHistory(8);
    rows[4] = baseRow(4, {
      id: "squatter",
      lane: 1,
      active_lanes: [1],
      active_lane_colors: [1],
      is_root: true,
    });
    const hit = renderer.getGraphHitAtPoint(
      renderer.getLaneX(1),
      probeY(renderer, 4),
      rows,
      0,
      rows.length,
    );
    expect(hit?.kind).toBe("node");
    expect(hit?.row.id).toBe("squatter");
  });

  it("survives hostile lane values and degenerate geometry without throwing", () => {
    const renderer = new GraphRenderer();
    const rows = closingHistory(8);
    rows[1].connections.push(
      { from_lane: Number.NaN, to_lane: 0, to_row_offset: 3, is_merge: false, color_index: 0 },
      { from_lane: -4, to_lane: 2, to_row_offset: 2, is_merge: false, color_index: 0 },
      { from_lane: 1, to_lane: 0, to_row_offset: Number.NaN, is_merge: false, color_index: 0 },
      { from_lane: 1, to_lane: 0, to_row_offset: -5, is_merge: false, color_index: 0 },
    );
    for (const x of [-50, 0, 35, 1e9, Number.NaN]) {
      for (const row of [0, 3, 7]) {
        expect(() =>
          renderer.getGraphHitAtPoint(x, probeY(renderer, row), rows, 0, rows.length),
        ).not.toThrow();
      }
    }
    // The legitimate connector still resolves through the noise.
    const hit = renderer.getGraphHitAtPoint(
      renderer.getLaneX(1),
      probeY(renderer, 4),
      rows,
      0,
      rows.length,
    );
    expect(hit?.kind).toBe("connector");
    expect(hit?.row.id).toBe("closer");
  });

  it("reports avatar hits with their own kind", () => {
    const renderer = new GraphRenderer();
    const rows = closingHistory(8);
    const avatarX = 300;
    const hit = renderer.getGraphHitAtPoint(
      avatarX,
      probeY(renderer, 3),
      rows,
      0,
      rows.length,
      0,
      avatarX,
    );
    expect(hit?.kind).toBe("avatar");
    expect(hit?.row.id).toBe("c3");
  });
});
