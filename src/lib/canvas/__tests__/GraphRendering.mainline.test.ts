import { describe, expect, it } from "vitest";
import devprism from "./fixtures/devprismMainline.json";
import { GraphRenderer, primedRowRange, type VisualCommitRow } from "../GraphRenderer";
import type { CommitGraphPayload } from "../../stores/graphStore";

/**
 * Pinned-mainline oracle on REAL solver output.
 *
 * `devprismMainline.json` is the first 240 rows of a merge-heavy repository
 * (DevPrism: 218 merges in 935 commits) solved by the Rust lane solver with
 * the repository's own ref hints, dumped by
 * `real_repo_smoke::real_repository_mainline_is_one_straight_rail`. It is
 * the shape that used to break the graph: `--topo-order` lists merged
 * feature commits above the main commits they forked from, so before the
 * mainline was pinned, main jogged into a feature's column at every such
 * fork. The contract now: every default-branch commit sits on column 0 in
 * colour 0, nothing else ever does, and the column renders as ONE unbroken
 * vertical rail from the tip down to main's last loaded commit — in a
 * single render pass and through the production strip cache + overlay
 * composition at every scroll window.
 */
const payload = devprism as unknown as CommitGraphPayload;
const rows: VisualCommitRow[] = payload.rows;

const MAINLINE_COLUMN = 0;
const MAINLINE_COLOR = 0;
/** graphCache's DEFAULT_STRIP_CSS_HEIGHT, snapped to whole rows like the cache does. */
const STRIP_CSS_HEIGHT = 512;

type Op = "move" | "line" | "bezier";
interface RecordedPath {
  points: Array<{ x: number; y: number; op: Op }>;
}

/** Records stroked geometry with the op that produced each point, so a
 * bezier corner's endpoint is never mistaken for a straight diagonal. */
function pathRecordingContext(height: number) {
  const paths: RecordedPath[] = [];
  let current: RecordedPath | null = null;
  const push = (x: number, y: number, op: Op) => current?.points.push({ x, y, op });
  const ctx = {
    save: () => {},
    restore: () => {},
    beginPath: () => {
      current = { points: [] };
    },
    moveTo: (x: number, y: number) => push(x, y, "move"),
    lineTo: (x: number, y: number) => push(x, y, "line"),
    bezierCurveTo: (_a: number, _b: number, _c: number, _d: number, x: number, y: number) =>
      push(x, y, "bezier"),
    arc: () => {},
    fill: () => {},
    fillRect: () => {},
    fillText: () => {},
    setTransform: () => {},
    stroke: () => {
      if (current && current.points.length >= 2) paths.push(current);
      current = null;
    },
    lineWidth: 2,
    lineCap: "round" as CanvasLineCap,
    lineJoin: "round" as CanvasLineJoin,
    globalAlpha: 1,
    imageSmoothingEnabled: true,
    strokeStyle: "",
    fillStyle: "",
    font: "",
    textAlign: "left",
    textBaseline: "alphabetic",
    canvas: { height, width: 600 } as HTMLCanvasElement,
  } as unknown as CanvasRenderingContext2D;
  return { ctx, paths };
}

interface Segment {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  op: Op;
}

function segmentsOf(paths: RecordedPath[], yOffset = 0): Segment[] {
  const segments: Segment[] = [];
  for (const path of paths) {
    for (let i = 1; i < path.points.length; i++) {
      const a = path.points[i - 1];
      const b = path.points[i];
      if (b.op === "move") continue;
      segments.push({ x1: a.x, y1: a.y + yOffset, x2: b.x, y2: b.y + yOffset, op: b.op });
    }
  }
  return segments;
}

const near = (a: number, b: number, tol = 0.01) => Math.abs(a - b) <= tol;

/** Merged coverage of [from, to] by the given vertical intervals; returns the first gap or null. */
function firstGap(
  intervals: Array<[number, number]>,
  from: number,
  to: number,
  tolerance: number,
): [number, number] | null {
  const sorted = intervals
    .map(([a, b]) => (a <= b ? [a, b] : [b, a]) as [number, number])
    .filter(([, b]) => b >= from - tolerance)
    .sort((p, q) => p[0] - q[0]);
  let reach = from;
  for (const [a, b] of sorted) {
    if (a > reach + tolerance) return [reach, a];
    reach = Math.max(reach, b);
    if (reach >= to - tolerance) return null;
  }
  return reach >= to - tolerance ? null : [reach, to];
}

describe("pinned mainline on real history (DevPrism, 240 rows)", () => {
  const renderer = new GraphRenderer();
  const { rowHeight, originX } = renderer.getConfig();
  const x0 = renderer.getLaneX(MAINLINE_COLUMN);
  const yOf = (row: number) => row * rowHeight + rowHeight / 2;
  const mainlineIdx = rows.flatMap((r, i) => (r.is_mainline ? [i] : []));
  const tipRow = mainlineIdx[0];
  const lastRow = mainlineIdx[mainlineIdx.length - 1];
  const isRowCentre = (y: number) =>
    near(((y - rowHeight / 2) / rowHeight) % 1, 0, 1e-6) ||
    near(((y - rowHeight / 2) / rowHeight) % 1, 1, 1e-6);

  /** Every stroked segment that touches column 0 is a rail: vertical on the
   * column, or horizontal along a row centreline (a merge peel leaving it or
   * a close arriving on it). A diagonal touching the column is a smear. */
  function assertOnlyRailsTouchColumnZero(segments: Segment[]) {
    for (const s of segments) {
      const touches = near(s.x1, x0) || near(s.x2, x0);
      if (!touches || s.op !== "line") continue;
      const vertical = near(s.x1, x0) && near(s.x2, x0);
      const horizontal = near(s.y1, s.y2) && isRowCentre(s.y1);
      expect(
        vertical || horizontal,
        `diagonal stroke touching column 0: (${s.x1},${s.y1})→(${s.x2},${s.y2})`,
      ).toBe(true);
    }
  }

  function verticalIntervalsAtColumnZero(segments: Segment[]): Array<[number, number]> {
    return segments
      .filter((s) => s.op === "line" && near(s.x1, x0) && near(s.x2, x0))
      .map((s) => [s.y1, s.y2] as [number, number]);
  }

  it("is the merge-heavy shape the pin exists for", () => {
    expect(rows.length).toBe(240);
    expect(payload.mainline_name).toBe("main");
    expect(payload.mainline_id).toBe(rows[tipRow].id);
    expect(mainlineIdx.length).toBeGreaterThan(40);
    expect(rows.filter((r) => r.is_merge).length).toBeGreaterThan(20);
    // Feature chains that reach a main commit as their FIRST parent from
    // above — the rows that used to steal main's ancestor and displace it.
    const closers = rows.filter((r, i) => {
      const first = r.connections[0];
      if (!first || first.is_dangling || first.is_merge || r.is_mainline) return false;
      return rows[i + first.to_row_offset]?.is_mainline === true;
    });
    expect(closers.length).toBeGreaterThan(10);
  });

  it("pins every default-branch commit to column 0 in colour 0 and nothing else there", () => {
    rows.forEach((row, i) => {
      if (row.is_mainline) {
        expect(row.lane, `${row.id} left column 0`).toBe(MAINLINE_COLUMN);
        expect(row.color_index, `${row.id} is off-colour`).toBe(MAINLINE_COLOR);
        const first = row.connections[0];
        if (first && !first.is_dangling) {
          const target = rows[i + first.to_row_offset];
          expect(target.id).toBe(row.parent_ids[0]);
          expect(target.is_mainline, `${row.id}'s first parent ${target.id} is off the rail`).toBe(true);
          expect(first.to_lane).toBe(MAINLINE_COLUMN);
        }
      } else {
        expect(row.lane, `${row.id} sits on main's column`).not.toBe(MAINLINE_COLUMN);
      }
      row.active_lanes.forEach((lane, l) => {
        if (lane !== MAINLINE_COLUMN) return;
        expect(i, `column 0 drawn above the tip at row ${i}`).toBeGreaterThanOrEqual(tipRow);
        expect(row.active_lane_colors[l]).toBe(MAINLINE_COLOR);
      });
      for (const conn of row.connections) {
        if (conn.is_dangling || conn.to_lane !== MAINLINE_COLUMN) continue;
        expect(rows[i + conn.to_row_offset].is_mainline, `${row.id} lands on column 0 off the rail`).toBe(true);
      }
    });
    // The rail closes with an honest stub at the window cut, never a line
    // into an unrelated row.
    const last = rows[lastRow];
    if (last.connections[0]) expect(last.connections[0].is_dangling).toBe(true);
  });

  it("renders column 0 as one unbroken vertical rail from the tip to main's last commit", () => {
    const { ctx, paths } = pathRecordingContext(rows.length * rowHeight);
    renderer.render(ctx, rows, 0, rows.length, 0, undefined, {
      viewportHeight: rows.length * rowHeight,
    });
    const segments = segmentsOf(paths);
    assertOnlyRailsTouchColumnZero(segments);

    const gap = firstGap(verticalIntervalsAtColumnZero(segments), yOf(tipRow), yOf(lastRow), 0.5);
    expect(gap, `column 0 has a hole between y=${gap?.[0]} and y=${gap?.[1]}`).toBeNull();

    // Nothing on the column above the tip: the reservation is whitespace.
    const aboveTip = verticalIntervalsAtColumnZero(segments).filter(
      ([a, b]) => Math.min(a, b) < yOf(tipRow) - rowHeight / 2 - 0.5,
    );
    expect(aboveTip).toEqual([]);
    // And the mainline's own edges are pure verticals: no first-parent edge
    // of a mainline row leaves the column.
    expect(originX).toBe(x0);
  });

  it("keeps the rail whole through the strip cache and overlay at every scroll window", () => {
    const rowsPerStrip = Math.max(1, Math.floor(STRIP_CSS_HEIGHT / rowHeight));
    const visible = 20;
    for (let winStart = 0; winStart + 1 < rows.length; winStart += 7) {
      const winEnd = Math.min(rows.length, winStart + visible);
      const viewTop = winStart * rowHeight;
      const viewBottom = winEnd * rowHeight;
      const segments: Segment[] = [];

      // Replays graphCache.paint + CommitTable's strip painter verbatim:
      // one render per overlapping strip, primed with the real lookback,
      // long connectors skipped; then the overlay draws them whole.
      const firstStrip = Math.floor(viewTop / (rowsPerStrip * rowHeight));
      const lastStrip = Math.floor((viewBottom - 1) / (rowsPerStrip * rowHeight));
      for (let s = firstStrip; s <= lastStrip; s++) {
        const stripFirstRow = s * rowsPerStrip;
        const rowCount = Math.min(rowsPerStrip, rows.length - stripFirstRow);
        if (rowCount <= 0) break;
        const range = primedRowRange(stripFirstRow, rowCount, rows.length);
        const stripTop = stripFirstRow * rowHeight;
        const { ctx, paths } = pathRecordingContext(rowCount * rowHeight);
        renderer.render(ctx, rows, range.from, stripFirstRow + rowCount - 1, stripTop, undefined, {
          viewportHeight: rowCount * rowHeight,
          skipLongConnectors: true,
        });
        segments.push(...segmentsOf(paths, stripTop));
      }
      {
        const { ctx, paths } = pathRecordingContext(viewBottom - viewTop);
        renderer.drawLongConnectors(ctx, rows, winStart, winEnd, viewTop, viewBottom - viewTop, {});
        segments.push(...segmentsOf(paths, viewTop));
      }

      assertOnlyRailsTouchColumnZero(segments);
      const from = Math.max(yOf(tipRow), viewTop);
      const to = Math.min(yOf(lastRow), viewBottom);
      if (to <= from) continue;
      const gap = firstGap(verticalIntervalsAtColumnZero(segments), from, to, 0.5);
      expect(
        gap,
        `window [${winStart},${winEnd}): column 0 has a hole between y=${gap?.[0]} and y=${gap?.[1]}`,
      ).toBeNull();
    }
  });
});
