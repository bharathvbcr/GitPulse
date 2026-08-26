import { describe, expect, it } from "vitest";
import { GraphRenderer, type VisualCommitRow } from "../GraphRenderer";

/**
 * Rendered-lane stability contract (GitKraken-style).
 *
 * Written against the OLD per-row display packing to demonstrate its
 * defects before the stable-column rework landed: packing renamed lanes on
 * every row, so a branch's rendered x slid sideways whenever a lower
 * neighbour died — the stair-step "weird branch" artifacts — and
 * hit-testing moved with it.
 *
 * The contract: a solver lane is a stable column. Its x never depends on
 * which other lanes happen to be alive on a given row. Pass-through lines
 * are perfectly vertical; connectors bend exactly once.
 */

interface RecordedPath {
  points: Array<{ x: number; y: number }>;
}

function pathRecordingContext() {
  const paths: RecordedPath[] = [];
  let current: RecordedPath | null = null;
  const start = () => {
    current = { points: [] };
    paths.push(current);
  };
  const push = (x: number, y: number) => current?.points.push({ x, y });
  return {
    paths,
    ctx: {
      save: () => {},
      restore: () => {},
      beginPath: start,
      moveTo: push,
      lineTo: push,
      bezierCurveTo: (
        _a: number,
        _b: number,
        _c: number,
        _d: number,
        x: number,
        y: number,
      ) => push(x, y),
      arc: () => {},
      fill: () => {},
      stroke: () => {
        current = null;
      },
      lineWidth: 2,
      lineCap: "round" as CanvasLineCap,
      lineJoin: "round" as CanvasLineJoin,
      globalAlpha: 1,
      imageSmoothingEnabled: true,
      strokeStyle: "",
      fillStyle: "",
      canvas: { height: 720, width: 400 } as HTMLCanvasElement,
    } as unknown as CanvasRenderingContext2D,
  };
}

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
 * A branch on lane 2 lives across six rows while a middle lane (1) dies at
 * row 2. Solver output in the stable-column world: lane 2 keeps index 2 the
 * whole time; rows 2+ simply have a hole at column 1.
 */
function neighbourDiesHistory(): VisualCommitRow[] {
  return [
    baseRow(0, {
      lane: 0,
      active_lanes: [0, 1, 2],
      active_lane_colors: [0, 1, 2],
      connections: [
        { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 },
      ],
      parent_ids: ["c1"],
    }),
    baseRow(1, {
      lane: 1,
      color_index: 1,
      active_lanes: [0, 1, 2],
      active_lane_colors: [0, 1, 2],
      is_root: true,
    }),
    baseRow(2, {
      lane: 0,
      active_lanes: [0, 2],
      active_lane_colors: [0, 2],
      connections: [
        { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 },
      ],
      parent_ids: ["c3"],
    }),
    baseRow(3, {
      lane: 0,
      active_lanes: [0, 2],
      active_lane_colors: [0, 2],
      connections: [
        { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 },
      ],
      parent_ids: ["c4"],
    }),
    baseRow(4, {
      lane: 0,
      active_lanes: [0, 2],
      active_lane_colors: [0, 2],
      is_root: true,
    }),
    baseRow(5, {
      lane: 2,
      color_index: 2,
      active_lanes: [2],
      active_lane_colors: [2],
      is_root: true,
    }),
  ];
}

describe("stable columns: a lane's x never depends on its neighbours", () => {
  it("keeps a live branch at the same x when a lower neighbour dies", () => {
    const renderer = new GraphRenderer();
    const rows = neighbourDiesHistory();
    const { ctx, paths } = pathRecordingContext();
    renderer.render(ctx, rows, 0, rows.length, 0, undefined, { viewportHeight: 720 });

    // Every stroked polyline must be either perfectly vertical (a
    // pass-through or straight connector) or bend between exactly two x
    // values (a single-corner connector). A third distinct x, or a
    // return to an earlier x, is the stair-step artifact.
    for (const p of paths.filter((q) => q.points.length >= 2)) {
      const xs: number[] = [];
      for (const pt of p.points) {
        if (xs.length === 0 || Math.abs(xs[xs.length - 1] - pt.x) >= 0.5) xs.push(pt.x);
      }
      expect(
        xs.length,
        `a stroked path visited ${xs.length} distinct x positions (${xs.join(", ")}) — lanes must not jog`,
      ).toBeLessThanOrEqual(2);
    }

    // The lane-2 branch specifically: every path segment drawn in its
    // colour sits at one single x for the branch's whole life.
    const lane2X = renderer.getLaneX(2);
    for (const p of paths.filter((q) => q.points.length >= 2)) {
      const touchesLane2 = p.points.some((pt) => Math.abs(pt.x - lane2X) < 0.5);
      const leavesLane2 = p.points.some((pt) => Math.abs(pt.x - lane2X) >= 0.5);
      expect(
        touchesLane2 && leavesLane2,
        "the lane-2 pass-through drifted off its own column",
      ).toBe(false);
    }
  });

  it("hit-tests a branch at its own column regardless of dead neighbours", () => {
    const renderer = new GraphRenderer();
    const rows = neighbourDiesHistory();
    const { rowHeight } = renderer.getConfig();
    const lane2X = renderer.getLaneX(2);

    // Row 3: lane 1 is dead, lane 2 still alive. Hovering the branch's own
    // column must resolve to its occupant (c5), on every row it crosses.
    const hit = renderer.getCommitAtPoint(lane2X, 3 * rowHeight + rowHeight / 2, rows, 0, rows.length);
    expect(hit?.id, "branch not hittable at its own stable column").toBe("c5");
  });

  it("sizes the gutter to the highest occupied column, holes included", () => {
    const renderer = new GraphRenderer();
    const rows = neighbourDiesHistory();
    const { originX, laneWidth, nodeRadius } = renderer.getConfig();
    // Lane 2 exists on every row; the gutter must reach it even on rows
    // where lane 1 is a hole.
    expect(renderer.measureWidth(rows)).toBeGreaterThanOrEqual(
      originX + 2 * laneWidth + nodeRadius,
    );
  });

  it("hit-tests a pass-through lane whose occupant is thousands of rows away", () => {
    // A merge at row 0 pulls in a branch whose only commit sits 3000 rows
    // below: every intermediate row carries the reservation in
    // active_lanes. Hovering that lane ANYWHERE along the span must name
    // the far occupant — a bounded walk that gives up and reports "no
    // commit here" is indistinguishable from an honest miss, which is the
    // capped-sample failure mode this test forbids.
    const renderer = new GraphRenderer();
    const N = 3002;
    const rows: VisualCommitRow[] = [];
    rows.push(
      baseRow(0, {
        lane: 0,
        is_merge: true,
        parent_ids: ["c1", "far"],
        connections: [
          { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 },
          { from_lane: 0, to_lane: 1, to_row_offset: N - 1, is_merge: true, color_index: 1 },
        ],
      }),
    );
    for (let i = 1; i < N - 1; i++) {
      rows.push(
        baseRow(i, {
          lane: 0,
          active_lanes: [0, 1],
          active_lane_colors: [0, 1],
          parent_ids: [`c${i + 1}`],
          connections: [
            { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 },
          ],
        }),
      );
    }
    rows.push(
      baseRow(N - 1, {
        id: "far",
        lane: 1,
        color_index: 1,
        active_lanes: [0, 1],
        active_lane_colors: [0, 1],
        is_root: true,
      }),
    );

    const { rowHeight } = renderer.getConfig();
    const laneX = renderer.getLaneX(1);
    for (const probeRow of [5, 700, 1500, 2900]) {
      const hit = renderer.getCommitAtPoint(
        laneX,
        probeRow * rowHeight + rowHeight / 2,
        rows,
        0,
        rows.length,
      );
      expect(
        hit?.id,
        `hovering lane 1 at row ${probeRow} must resolve the far occupant`,
      ).toBe("far");
    }
  });

  it("resolves pass-through occupants identically to a brute-force reference", () => {
    // Differential check of the occupancy index: on randomized sparse
    // occupancy, hovering a pass-through lane must name the commit an
    // exhaustive nearest-row scan names (ties prefer the row below, where
    // the line is heading). Any cap, off-by-one, or cache staleness in the
    // index shows up as a divergence.
    const renderer = new GraphRenderer();
    const { rowHeight } = renderer.getConfig();
    let state = 0xc0ffee >>> 0;
    const rand = () => {
      state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
      return state / 4294967296;
    };
    const N = 4000;
    const rows: VisualCommitRow[] = [];
    for (let i = 0; i < N; i++) {
      // Lane 1 is occupied rarely (~1/200 rows) but always live; lane 0
      // carries everything else, so most probes must reach far.
      const onLaneOne = rand() < 0.005;
      rows.push(
        baseRow(i, {
          lane: onLaneOne ? 1 : 0,
          color_index: onLaneOne ? 1 : 0,
          active_lanes: [0, 1],
          active_lane_colors: [0, 1],
          is_root: true,
        }),
      );
    }
    const bruteForce = (fromIdx: number): string | null => {
      let best: number | null = null;
      for (let d = 0; d < N; d++) {
        const down = fromIdx + d;
        if (down < N && rows[down].lane === 1) {
          best = down;
          break;
        }
        const up = fromIdx - d;
        if (d > 0 && up >= 0 && rows[up].lane === 1) {
          best = up;
          break;
        }
      }
      return best === null ? null : rows[best].id;
    };
    const laneOneX = renderer.getLaneX(1);
    for (let probe = 0; probe < N; probe += 7) {
      const hit = renderer.getCommitAtPoint(
        laneOneX,
        probe * rowHeight + rowHeight / 2,
        rows,
        0,
        N,
      );
      const expected = rows[probe].lane === 1 ? rows[probe].id : bruteForce(probe);
      expect(hit?.id ?? null, `probe at row ${probe} diverged from reference`).toBe(
        expected,
      );
    }
  });

  it("survives hostile lane values in the occupancy index without throwing", () => {
    const renderer = new GraphRenderer();
    const { rowHeight } = renderer.getConfig();
    const rows: VisualCommitRow[] = [
      baseRow(0, { lane: Number.NaN, active_lanes: [Number.NaN, 0] }),
      baseRow(1, { lane: -5, active_lanes: [0] }),
      baseRow(2, { lane: 2, active_lanes: [0, 2], active_lane_colors: [0, 2] }),
      baseRow(3, {
        lane: 0,
        active_lanes: [0, 2, Number.POSITIVE_INFINITY],
        active_lane_colors: [0, 2, 3],
      }),
    ];
    for (let r = 0; r < rows.length; r++) {
      for (const lane of [-3, 0, 1, 2, 500, Number.NaN]) {
        const x = Number.isFinite(lane) ? renderer.getLaneX(lane) : Number.NaN;
        expect(() =>
          renderer.getCommitAtPoint(x, r * rowHeight + rowHeight / 2, rows, 0, rows.length),
        ).not.toThrow();
      }
    }
    // The finite occupant is still found through the noise.
    const hit = renderer.getCommitAtPoint(
      renderer.getLaneX(2),
      3 * rowHeight + rowHeight / 2,
      rows,
      0,
      rows.length,
    );
    expect(hit?.id).toBe("c2");
  });

  it("skips a dangling stub hidden under a live line on the same lane", () => {
    // A merge whose mainline left the window: the solver promotes the
    // surviving parent straight down the merge's own column. The stub for
    // the cut-off parent would be stroked ON TOP of that solid live line —
    // translucent overdraw that muddies the lane without conveying
    // anything. Covered stubs must not be drawn; uncovered ones must.
    const renderer = new GraphRenderer();
    const rows: VisualCommitRow[] = [
      baseRow(0, {
        lane: 0,
        is_merge: true,
        parent_ids: ["ghost", "p"],
        connections: [
          {
            from_lane: 0,
            to_lane: 0,
            to_row_offset: 1,
            is_merge: false,
            color_index: 0,
            is_dangling: true,
          },
          { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: true, color_index: 0 },
        ],
      }),
      baseRow(1, { lane: 0, is_root: true }),
      baseRow(2, {
        lane: 0,
        parent_ids: ["cut"],
        connections: [
          {
            from_lane: 0,
            to_lane: 0,
            to_row_offset: 1,
            is_merge: false,
            color_index: 0,
            is_dangling: true,
          },
        ],
        is_root: false,
      }),
    ];
    const { ctx, paths } = pathRecordingContext();
    renderer.drawDanglingStubs(ctx, rows, 0, rows.length, 0, 720);

    const { rowHeight } = renderer.getConfig();
    const y0 = renderer.getRowY(0);
    const y2 = renderer.getRowY(2);
    const strokesNear = (y: number) =>
      paths.filter(
        (p) =>
          p.points.length >= 2 &&
          p.points.every((pt) => pt.y >= y - 0.5 && pt.y <= y + rowHeight),
      ).length;
    expect(
      strokesNear(y0),
      "row 0's stub is fully covered by the promoted live line and must not draw",
    ).toBe(0);
    expect(
      strokesNear(y2),
      "row 2's stub is uncovered and must still draw",
    ).toBeGreaterThan(0);
  });

  it("draws a connector between stable columns with exactly one bend", () => {
    const renderer = new GraphRenderer();
    // Merge commit on lane 0, merged-in branch on lane 3, with a hole at
    // lane 1 opening mid-span: the edge must run child → corner → straight
    // vertical on lane 3, never tracking per-row occupancy.
    const rows: VisualCommitRow[] = [
      baseRow(0, {
        lane: 0,
        is_merge: true,
        parent_ids: ["c4", "c3"],
        active_lanes: [0, 1],
        active_lane_colors: [0, 1],
        connections: [
          { from_lane: 0, to_lane: 0, to_row_offset: 4, is_merge: false, color_index: 0 },
          { from_lane: 0, to_lane: 3, to_row_offset: 3, is_merge: true, color_index: 3 },
        ],
      }),
      baseRow(1, {
        lane: 1,
        color_index: 1,
        active_lanes: [0, 1, 3],
        active_lane_colors: [0, 1, 3],
        is_root: true,
      }),
      baseRow(2, { lane: 0, active_lanes: [0, 3], active_lane_colors: [0, 3] }),
      baseRow(3, { lane: 3, color_index: 3, active_lanes: [0, 3], active_lane_colors: [0, 3], is_root: true }),
      baseRow(4, { lane: 0, active_lanes: [0], active_lane_colors: [0], is_root: true }),
    ];
    const { ctx, paths } = pathRecordingContext();
    renderer.render(ctx, rows, 0, rows.length, 0, undefined, { viewportHeight: 720 });

    const lane3X = renderer.getLaneX(3);
    const mergePaths = paths.filter(
      (p) => p.points.length >= 2 && p.points.some((pt) => Math.abs(pt.x - lane3X) < 0.5),
    );
    expect(mergePaths.length, "merge edge to lane 3 was not drawn").toBeGreaterThan(0);
    for (const p of mergePaths) {
      const xs: number[] = [];
      for (const pt of p.points) {
        if (xs.length === 0 || Math.abs(xs[xs.length - 1] - pt.x) >= 0.5) xs.push(pt.x);
      }
      expect(
        xs.length,
        `merge edge visited x positions ${xs.join(", ")} — must bend exactly once`,
      ).toBeLessThanOrEqual(2);
    }
  });
});
