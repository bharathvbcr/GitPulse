import { describe, expect, it } from "vitest";
import { GraphRenderer, type VisualCommitRow } from "../GraphRenderer";
import { makeRecordingCtx, row } from "./recordingCtx";
import scholarlmConfluence from "./fixtures/scholarlmConfluence.json";

/**
 * Connector geometry contract: GitKraken-style rails.
 *
 * A lane-changing connector is a straight vertical run plus a straight
 * horizontal run joined by ONE tight quarter-turn, never a diagonal smear
 * across the span:
 *
 * - a merge peel travels horizontally ALONG ITS CHILD'S ROW, turns once,
 *   then descends its own column;
 * - a closing edge descends its own column, turns once, then approaches
 *   ALONG ITS PARENT'S ROW;
 * - the turn's vertical extent never exceeds half a row, so everything
 *   between the two end bands is pure vertical;
 * - every edge landing on one row shares the same horizontal approach y,
 *   so simultaneous landings coincide instead of braiding into crossing
 *   diagonals — the defect photographed on the scholarlm confluences.
 *
 * These tests sample the actual recorded path (lines and cubics alike) so
 * the grammar is asserted on geometry, not on which canvas ops were used.
 */

interface Pt {
  x: number;
  y: number;
}

type Recorded = Record<string, unknown>;

/**
 * Stroked linear paths (beginPath..stroke), excluding anything containing an
 * arc: node discs, cutouts, and emphasis rings are not connector geometry.
 */
function strokedPaths(calls: Recorded[]): Recorded[][] {
  const paths: Recorded[][] = [];
  let current: Recorded[] | null = null;
  let hasArc = false;
  for (const c of calls) {
    switch (c.op) {
      case "beginPath":
        current = [];
        hasArc = false;
        break;
      case "moveTo":
      case "lineTo":
      case "bezier":
        current?.push(c);
        break;
      case "arc":
        hasArc = true;
        break;
      case "stroke":
        if (current && current.length > 0 && !hasArc) paths.push(current);
        current = null;
        break;
      case "fill":
        current = null;
        break;
    }
  }
  return paths;
}

/**
 * Flatten one path into sampled points. Sampling is adaptive — at most one
 * point every half pixel, bounded — so a sub-pixel tolerance can never miss
 * a segment merely because it was long.
 */
function samplePath(ops: Recorded[]): Pt[] {
  const pts: Pt[] = [];
  let cx = 0;
  let cy = 0;
  for (const o of ops) {
    if (o.op === "moveTo") {
      cx = o.x as number;
      cy = o.y as number;
      pts.push({ x: cx, y: cy });
    } else if (o.op === "lineTo") {
      const x = o.x as number;
      const y = o.y as number;
      const len = Math.hypot(x - cx, y - cy);
      const n = Math.min(2048, Math.max(16, Math.ceil(len * 2)));
      for (let t = 1; t <= n; t++) {
        pts.push({ x: cx + ((x - cx) * t) / n, y: cy + ((y - cy) * t) / n });
      }
      cx = x;
      cy = y;
    } else if (o.op === "bezier") {
      const [a, b, c2, d, e, f] = [o.a, o.b, o.c, o.d, o.e, o.f] as number[];
      const chord = Math.hypot(e - cx, f - cy);
      const n = Math.min(512, Math.max(32, Math.ceil(chord * 2)));
      for (let t = 1; t <= n; t++) {
        const s = t / n;
        const u = 1 - s;
        pts.push({
          x: u * u * u * cx + 3 * u * u * s * a + 3 * u * s * s * c2 + s * s * s * e,
          y: u * u * u * cy + 3 * u * u * s * b + 3 * u * s * s * d + s * s * s * f,
        });
      }
      cx = e;
      cy = f;
    }
  }
  return pts;
}

function xSpread(pts: Pt[]): number {
  let min = Infinity;
  let max = -Infinity;
  for (const p of pts) {
    if (p.x < min) min = p.x;
    if (p.x > max) max = p.x;
  }
  return max - min;
}

/**
 * The universal rail envelope: outside a half-row band at each end, the
 * path is one straight vertical. Any x-travel in the interior is a
 * diagonal smear.
 */
function assertRailEnvelope(pts: Pt[], rowHeight: number, label: string): void {
  for (let i = 1; i < pts.length; i++) {
    expect(pts[i].y, `${label}: y must never reverse`).toBeGreaterThanOrEqual(pts[i - 1].y - 0.51);
  }
  const band = rowHeight / 2 + 0.75;
  const y0 = pts[0].y;
  const y1 = pts[pts.length - 1].y;
  const interior = pts.filter((p) => p.y > y0 + band && p.y < y1 - band);
  if (interior.length === 0) return;
  expect(
    xSpread(interior),
    `${label}: interior x-travel means the connector smears diagonally instead of running straight`,
  ).toBeLessThanOrEqual(0.75);
}

function sampleNear(paths: Pt[][], x: number, y: number, tol = 1.0): boolean {
  return paths.some((pts) => pts.some((p) => Math.abs(p.x - x) <= tol && Math.abs(p.y - y) <= tol));
}

const THEME = {
  background: "#ffffff",
  nodeStroke: "#dddddd",
  selection: "#0000ff",
  head: "#111111",
  muted: "#888888",
};

function renderPaths(renderer: GraphRenderer, rows: VisualCommitRow[], lo = 0, hi = rows.length, scroll = 0) {
  const { ctx, calls } = makeRecordingCtx(4000);
  renderer.render(ctx, rows, lo, hi, scroll, undefined, { theme: THEME, viewportHeight: 4000 });
  return strokedPaths(calls).map(samplePath);
}

describe("connector rails", () => {
  it("runs a wide merge peel horizontally along the merge row, then straight down its own column", () => {
    const renderer = new GraphRenderer();
    const { rowHeight } = renderer.getConfig();
    const rows = [
      row({
        id: "merge",
        lane: 0,
        active_lanes: [],
        is_merge: true,
        connections: [
          { from_lane: 0, to_lane: 8, to_row_offset: 3, is_merge: true, color_index: 1 },
        ],
      }),
      row({ id: "a", lane: 0, active_lanes: [] }),
      row({ id: "b", lane: 0, active_lanes: [] }),
      row({ id: "parent", lane: 8, active_lanes: [] }),
    ];
    const paths = renderPaths(renderer, rows);
    const connector = paths.find((pts) => xSpread(pts) > 1);
    expect(connector, "the peel must be drawn").toBeDefined();

    const xFrom = renderer.getLaneX(0);
    const xTo = renderer.getLaneX(8);
    const yFrom = renderer.getRowY(0, 0);

    // Horizontal travel happens on the child's row: the run at yFrom reaches
    // from the merge commit to within one turn radius of the target column.
    const atChildRow = connector!.filter((p) => Math.abs(p.y - yFrom) <= 0.5);
    expect(Math.min(...atChildRow.map((p) => p.x))).toBeLessThanOrEqual(xFrom + 0.5);
    expect(
      Math.max(...atChildRow.map((p) => p.x)),
      "the peel must run horizontally along the merge row to the target column",
    ).toBeGreaterThanOrEqual(xTo - rowHeight / 2 - 1);

    // Below the turn band the path sits exactly on the target column.
    for (const p of connector!) {
      if (p.y > yFrom + rowHeight / 2 + 0.75) {
        expect(Math.abs(p.x - xTo), "descent must hold the target column").toBeLessThanOrEqual(0.6);
      }
    }
    assertRailEnvelope(connector!, rowHeight, "wide merge peel");
  });

  it("descends a wide closing edge on its own column, approaching along the parent row", () => {
    const renderer = new GraphRenderer();
    const { rowHeight } = renderer.getConfig();
    const rows = [
      row({
        id: "tip",
        lane: 8,
        active_lanes: [],
        connections: [
          { from_lane: 8, to_lane: 0, to_row_offset: 4, is_merge: false, color_index: 1 },
        ],
      }),
      row({ id: "a", lane: 8, active_lanes: [] }),
      row({ id: "b", lane: 8, active_lanes: [] }),
      row({ id: "c", lane: 8, active_lanes: [] }),
      row({ id: "parent", lane: 0, active_lanes: [] }),
    ];
    const paths = renderPaths(renderer, rows);
    const connector = paths.find((pts) => xSpread(pts) > 1);
    expect(connector, "the closing edge must be drawn").toBeDefined();

    const xFrom = renderer.getLaneX(8);
    const xTo = renderer.getLaneX(0);
    const yTo = renderer.getRowY(4, 0);

    // Above the turn band the path sits exactly on the child's column.
    for (const p of connector!) {
      if (p.y < yTo - rowHeight / 2 - 0.75) {
        expect(Math.abs(p.x - xFrom), "descent must hold the child's column").toBeLessThanOrEqual(0.6);
      }
    }

    // Horizontal travel happens on the parent's row: the approach at yTo
    // spans from within one turn radius of the child's column to the parent.
    const atParentRow = connector!.filter((p) => Math.abs(p.y - yTo) <= 0.5);
    expect(Math.min(...atParentRow.map((p) => p.x))).toBeLessThanOrEqual(xTo + 0.5);
    expect(
      Math.max(...atParentRow.map((p) => p.x)),
      "the close must approach horizontally along the parent row",
    ).toBeGreaterThanOrEqual(xFrom - rowHeight / 2 - 1);
    assertRailEnvelope(connector!, rowHeight, "wide closing edge");
  });

  it("makes simultaneous landings coincide on the shared approach instead of braiding", () => {
    const renderer = new GraphRenderer();
    const { rowHeight } = renderer.getConfig();
    const rows = [
      row({
        id: "tip4",
        lane: 4,
        active_lanes: [],
        connections: [
          { from_lane: 4, to_lane: 0, to_row_offset: 5, is_merge: false, color_index: 1 },
        ],
      }),
      row({
        id: "tip7",
        lane: 7,
        active_lanes: [],
        connections: [
          { from_lane: 7, to_lane: 0, to_row_offset: 4, is_merge: false, color_index: 2 },
        ],
      }),
      row({
        id: "tip10",
        lane: 10,
        active_lanes: [],
        connections: [
          { from_lane: 10, to_lane: 0, to_row_offset: 3, is_merge: false, color_index: 3 },
        ],
      }),
      row({ id: "a", lane: 0, active_lanes: [] }),
      row({ id: "b", lane: 0, active_lanes: [] }),
      row({ id: "parent", lane: 0, active_lanes: [] }),
    ];
    const paths = renderPaths(renderer, rows);
    const yTo = renderer.getRowY(5, 0);
    // The corridor stops one turn radius short of each side: the parent's
    // turn-in and the nearest child's own quarter-turn legitimately curve
    // there. Everything between must be the shared flat approach.
    const corridorLo = renderer.getLaneX(0) + rowHeight / 2 + 1;
    const corridorHi = renderer.getLaneX(4) - rowHeight / 2 - 1;

    // Between the parent's column and the nearest child column, all three
    // approaches run at exactly the parent's row — one shared line, not a
    // fan of crossing diagonals.
    let corridorSamples = 0;
    for (const pts of paths) {
      if (xSpread(pts) <= 0.75) continue; // vertical lane runs are not landings
      for (const p of pts) {
        if (p.x >= corridorLo && p.x <= corridorHi) {
          corridorSamples += 1;
          expect(
            Math.abs(p.y - yTo),
            "every landing must cross the shared corridor at the parent row's y",
          ).toBeLessThanOrEqual(0.6);
        }
      }
    }
    expect(corridorSamples, "the corridor must actually be crossed").toBeGreaterThan(0);
  });

  it("holds the rail envelope for every connector under seeded fuzz across configs", () => {
    let state = 20260826 >>> 0;
    const rnd = () => {
      state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
      return state / 2 ** 32;
    };
    for (let iter = 0; iter < 100; iter++) {
      const rowHeight = 8 + Math.floor(rnd() * 41);
      const laneWidth = 6 + Math.floor(rnd() * 35);
      const renderer = new GraphRenderer({ rowHeight, laneWidth, originX: 12 });
      const fromLane = Math.floor(rnd() * 13);
      let toLane = Math.floor(rnd() * 13);
      if (toLane === fromLane) toLane = (toLane + 1) % 13;
      const offset = 1 + Math.floor(rnd() * 30);
      const isMerge = rnd() < 0.5;
      const rows: VisualCommitRow[] = [
        row({
          id: `child${iter}`,
          lane: fromLane,
          active_lanes: [],
          is_merge: isMerge,
          connections: [
            { from_lane: fromLane, to_lane: toLane, to_row_offset: offset, is_merge: isMerge, color_index: 1 },
          ],
        }),
      ];
      for (let i = 1; i < offset; i++) rows.push(row({ id: `mid${iter}-${i}`, lane: fromLane, active_lanes: [] }));
      rows.push(row({ id: `parent${iter}`, lane: toLane, active_lanes: [] }));

      const paths = renderPaths(renderer, rows);
      const connector = paths.find((pts) => xSpread(pts) > 1);
      expect(connector, `iter ${iter}: connector must be drawn`).toBeDefined();
      const first = connector![0];
      const last = connector![connector!.length - 1];
      const xFrom = renderer.getLaneX(fromLane);
      const xTo = renderer.getLaneX(toLane);
      // Endpoints are exact: the stroke starts on the child and ends on the parent.
      expect(Math.abs(first.x - xFrom)).toBeLessThanOrEqual(0.01);
      expect(Math.abs(first.y - renderer.getRowY(0, 0))).toBeLessThanOrEqual(0.01);
      expect(Math.abs(last.x - xTo)).toBeLessThanOrEqual(0.01);
      expect(Math.abs(last.y - renderer.getRowY(offset, 0))).toBeLessThanOrEqual(0.01);
      // Bounding box: no overshoot past either column or either row.
      const loX = Math.min(xFrom, xTo) - 0.51;
      const hiX = Math.max(xFrom, xTo) + 0.51;
      for (const p of connector!) {
        expect(p.x).toBeGreaterThanOrEqual(loX);
        expect(p.x).toBeLessThanOrEqual(hiX);
        expect(p.y).toBeGreaterThanOrEqual(first.y - 0.51);
        expect(p.y).toBeLessThanOrEqual(last.y + 0.51);
      }
      assertRailEnvelope(connector!, rowHeight, `iter ${iter} (rh=${rowHeight} lw=${laneWidth} ${fromLane}->${toLane} +${offset} merge=${isMerge})`);
    }
  }, 30_000);

  describe("scholarlm confluence fixture (real solver output behind the artifact screenshot)", () => {
    const rows = scholarlmConfluence as VisualCommitRow[];
    // Local indices of the two photographed confluences: 590cb23 is fixture
    // row 79 (lane 1), db91450 is fixture row 85 (lane 2).
    const CONFLUENCES = [79, 85];

    function fixtureWindowPaths(renderer: GraphRenderer) {
      const { rowHeight } = renderer.getConfig();
      const lo = 69;
      const hi = 87;
      const { ctx, calls } = makeRecordingCtx((hi - lo) * rowHeight);
      renderer.render(ctx, rows, lo, hi, lo * rowHeight, undefined, {
        theme: THEME,
        viewportHeight: (hi - lo) * rowHeight,
      });
      return { paths: strokedPaths(calls).map(samplePath), lo, hi };
    }

    it("draws every confluence landing as a rail that approaches on the parent row", () => {
      const renderer = new GraphRenderer();
      const { rowHeight } = renderer.getConfig();
      const { paths, lo } = fixtureWindowPaths(renderer);
      const scroll = lo * rowHeight;

      for (const target of CONFLUENCES) {
        const yTo = renderer.getRowY(target, scroll);
        for (let i = 0; i < rows.length; i++) {
          for (const conn of rows[i].connections) {
            if (conn.is_dangling || conn.is_merge) continue;
            if (i + conn.to_row_offset !== target) continue;
            if (conn.from_lane === conn.to_lane) continue;
            const xFrom = renderer.getLaneX(conn.from_lane);
            const xTo = renderer.getLaneX(conn.to_lane);
            const label = `close ${rows[i].id} lane${conn.from_lane}->lane${conn.to_lane}`;
            // The approach runs along the parent row from just past the turn.
            expect(
              sampleNear(paths, xFrom - rowHeight / 2 - 1, yTo, 2.5) ||
                sampleNear(paths, xFrom, yTo - rowHeight / 2, 2.5),
              `${label}: turn must happen at the child's own column`,
            ).toBe(true);
            expect(sampleNear(paths, xTo, yTo, 1.0), `${label}: must land on the parent`).toBe(true);
            // Midway across the horizontal gap the approach is AT the
            // parent row — this is what makes stacked landings coincide.
            const midX = (xFrom + xTo) / 2;
            expect(
              sampleNear(paths, midX, yTo, 1.0),
              `${label}: approach must run along the parent row, not smear diagonally`,
            ).toBe(true);
          }
        }
      }
    });

    it("draws every merge peel horizontally along its own row before descending", () => {
      const renderer = new GraphRenderer();
      const { rowHeight } = renderer.getConfig();
      const { paths, lo, hi } = fixtureWindowPaths(renderer);
      const scroll = lo * rowHeight;

      let peels = 0;
      for (let i = lo; i < hi; i++) {
        for (const conn of rows[i].connections) {
          if (conn.is_dangling || !conn.is_merge) continue;
          if (conn.from_lane === conn.to_lane) continue;
          const target = i + conn.to_row_offset;
          if (target >= rows.length) continue;
          peels += 1;
          const xFrom = renderer.getLaneX(conn.from_lane);
          const xTo = renderer.getLaneX(conn.to_lane);
          const yFrom = renderer.getRowY(i, scroll);
          const midX = (xFrom + xTo) / 2;
          expect(
            sampleNear(paths, midX, yFrom, 1.0),
            `peel ${rows[i].id} lane${conn.from_lane}->lane${conn.to_lane}: must run along its own row`,
          ).toBe(true);
          expect(
            sampleNear(paths, xTo, renderer.getRowY(target, scroll), 1.0),
            `peel ${rows[i].id}: must land on the merged-in tip`,
          ).toBe(true);
        }
      }
      expect(peels, "the fixture window contains merge peels").toBeGreaterThan(0);
    });

    it("keeps every drawn connector inside the rail envelope", () => {
      const renderer = new GraphRenderer();
      const { rowHeight } = renderer.getConfig();
      const { paths } = fixtureWindowPaths(renderer);
      let bending = 0;
      for (const pts of paths) {
        if (pts.length < 2 || xSpread(pts) <= 0.75) continue;
        bending += 1;
        assertRailEnvelope(pts, rowHeight, "fixture connector");
      }
      // The window is the photographed confluence region: it must contain a
      // healthy population of bending connectors, or the assertions above
      // ran against nothing.
      expect(bending).toBeGreaterThanOrEqual(10);
    });
  });
});
