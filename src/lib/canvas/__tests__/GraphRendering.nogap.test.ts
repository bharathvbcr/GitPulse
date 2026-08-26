import { describe, expect, it } from "vitest";
import {
  DENSITY_CONFIGS,
  GraphRenderer,
  LOOKBACK_ROWS,
  primedRowRange,
  type LaneConnection,
  type VisualCommitRow,
} from "../GraphRenderer";

/**
 * The no-gap oracle.
 *
 * Op counting proves "something drew"; it does not prove "the graph is
 * connected". This suite replays the EXACT production frame composition —
 * strip painter (short connectors, primed lookback, long skipped) plus live
 * overlay (long connectors whole) — over randomized histories and scroll
 * windows, then asserts as a geometric property:
 *
 *   for every non-dangling connection whose parent row renders inside the
 *   window, some stroked canvas path runs from the child's node centre to
 *   the parent's node centre.
 *
 * Spans deliberately straddle every ownership boundary (rowsPerStrip=14,
 * LOOKBACK_ROWS=60) so an off-by-one on either side of the split shows up
 * as a missing segment rather than a passing count.
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
      bezierCurveTo: (_a: number, _b: number, _c: number, _d: number, x: number, y: number) =>
        push(x, y),
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

const ROW_HEIGHT = DENSITY_CONFIGS.spacious.rowHeight;
/** Strip size used by the fake tiling below (matches ~14 spacious rows). */
const ROWS_PER_STRIP = Math.floor(512 / ROW_HEIGHT);

function laneX(lane: number): number {
  return DENSITY_CONFIGS.spacious.originX + lane * DENSITY_CONFIGS.spacious.laneWidth;
}

function yOf(rowIdx: number, scrollTop: number): number {
  return rowIdx * ROW_HEIGHT + ROW_HEIGHT / 2 - scrollTop;
}

/** Deterministic LCG — identical seeds yield identical histories. */
function lcg(seed: number): () => number {
  let state = seed >>> 0 || 1;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 4294967296;
  };
}

/**
 * History with one merge edge per row at boundary-critical spans: the edge
 * from row i targets row i + OFFSETS[i % len], cycling through values that
 * hug both sides of the strip-seam (14) and lookback (60) boundaries.
 */
const SPAN_CYCLE = [2, 13, 14, 15, 59, 60, 61, 62, 120, 777];

function boundaryHistory(n = 900): VisualCommitRow[] {
  const rows = Array.from({ length: n }, (_, i) => ({
    id: `c${i}`,
    parent_ids: [] as string[],
    summary: `c${i}`,
    author_name: "ada",
    author_email: "ada@example.com",
    timestamp: 1,
    lane: i % 3,
    color_index: i % 12,
    active_lanes: [i % 3],
    active_lane_colors: [i % 12],
    connections: [] as LaneConnection[],
    is_merge: false,
    is_root: false,
  }));
  const edges: Array<{ c: number; t: number }> = [];
  for (let i = 0; i + SPAN_CYCLE.length <= n; i++) {
    const span = SPAN_CYCLE[i % SPAN_CYCLE.length];
    const t = i + span;
    if (t >= n) continue;
    rows[i].parent_ids.push(`c${t}`);
    rows[i].connections.push({
      from_lane: rows[i].lane,
      to_lane: rows[t].lane,
      to_row_offset: span,
      is_merge: true,
      color_index: 3,
    });
    edges.push({ c: i, t });
  }
  void edges;
  // A few window-cut tips with dangling parents for honesty coverage.
  rows[0].connections.push({
    from_lane: 0,
    to_lane: 0,
    to_row_offset: 1,
    is_merge: false,
    color_index: 0,
    is_dangling: true,
  });

  // Seam-probe edges: child EXACTLY at a strip's primed start
  // (stripFirstRow - LOOKBACK_ROWS) with its parent deep enough inside the
  // strip that no neighbouring strip's render band can rescue the edge.
  // These are the cases where a single row of priming drift fails loudly.
  for (let k = 6; k * ROWS_PER_STRIP + 12 < n; k++) {
    const stripFirstRow = k * ROWS_PER_STRIP;
    const c = stripFirstRow - LOOKBACK_ROWS;
    const t = stripFirstRow + 8;
    if (c <= 0 || t >= n || t - c <= LOOKBACK_ROWS) continue;
    rows[c].parent_ids.push(`c${t}`);
    rows[c].connections.push({
      from_lane: rows[c].lane,
      to_lane: rows[t].lane,
      to_row_offset: t - c,
      is_merge: true,
      color_index: 4,
    });
  }
  return rows;
}

function closeEnough(a: number, b: number): boolean {
  return Math.abs(a - b) < 0.5;
}

describe("no-gap oracle: production frame composition", () => {
  /**
   * Replays the cached-frame composition EXACTLY as production issues it:
   * graphCache.paint walks every strip overlapping the viewport and calls
   * the painter once per strip (primed lookback above the strip's own rows,
   * viewport band = the strip's own pixel height), then the overlay draws
   * long connectors over the visible window.
   *
   * Each pass records with its own scroll offset because strips rasterize in
   * strip-local space; production translates them back at blit time.
   */
  function replayProductionFrame(
    renderer: GraphRenderer,
    rows: VisualCommitRow[],
    winStart: number,
    winEnd: number,
  ): Array<{ offset: number; paths: RecordedPath[] }> {
    const passes: Array<{ offset: number; paths: RecordedPath[] }> = [];

    const viewTopCss = winStart * ROW_HEIGHT;
    const viewBottomCss = winEnd * ROW_HEIGHT;
    const firstStrip = Math.floor(viewTopCss / (ROWS_PER_STRIP * ROW_HEIGHT));
    const lastStrip = Math.floor((viewBottomCss - 1) / (ROWS_PER_STRIP * ROW_HEIGHT));

    for (let s = firstStrip; s <= lastStrip; s++) {
      const stripFirstRow = s * ROWS_PER_STRIP;
      const rowCount = Math.min(ROWS_PER_STRIP, rows.length - stripFirstRow);
      if (rowCount <= 0) break;
      // CommitTable's painter: the REAL primedRowRange + skipLongConnectors,
      // so drills against production seam arithmetic are exercised verbatim.
      const range = primedRowRange(stripFirstRow, rowCount, rows.length);
      const stripTopCss = stripFirstRow * ROW_HEIGHT;
      const { ctx, paths } = pathRecordingContext();
      renderer.render(
        ctx,
        rows,
        range.from,
        stripFirstRow + rowCount - 1,
        stripTopCss,
        undefined,
        { viewportHeight: rowCount * ROW_HEIGHT, skipLongConnectors: true },
      );
      passes.push({ offset: stripTopCss, paths: paths.filter((p) => p.points.length >= 2) });
    }

    // Live overlay pass (graphComposite.ts, staticBlitted branch):
    {
      const { ctx, paths } = pathRecordingContext();
      renderer.drawLongConnectors(ctx, rows, winStart, winEnd, viewTopCss, viewBottomCss - viewTopCss, {});
      passes.push({ offset: viewTopCss, paths: paths.filter((p) => p.points.length >= 2) });
    }

    return passes;
  }

  function edgeDrawnWhole(
    passes: Array<{ offset: number; paths: RecordedPath[] }>,
    xFrom: number,
    yFromContent: number,
    xTo: number,
    yToContent: number,
  ): boolean {
    return passes.some(({ offset, paths }) =>
      paths.some((p) => {
        const dy = yFromContent - offset;
        const dy2 = yToContent - offset;
        return (
          closeEnough(p.points[0].x, xFrom) &&
          closeEnough(p.points[0].y, dy) &&
          closeEnough(p.points[p.points.length - 1].x, xTo) &&
          closeEnough(p.points[p.points.length - 1].y, dy2)
        );
      }),
    );
  }

  it("every in-window parent edge arrives whole from its own child", () => {
    const renderer = new GraphRenderer();
    const rows = boundaryHistory();

    // Windows aligned TO seams and misaligned by half-strip fractions, plus
    // deterministic pseudo-random offsets — both boundary classes exercised.
    const rand = lcg(42);
    const starts: number[] = [];
    for (let s = 0; s + 30 < rows.length; s += ROWS_PER_STRIP) starts.push(s);
    for (let k = 0; k < 40; k++) {
      starts.push(Math.floor(rand() * (rows.length - 40)));
    }

    for (const winStart of starts) {
      const winEnd = Math.min(rows.length, winStart + 25);
      const passes = replayProductionFrame(renderer, rows, winStart, winEnd);

      for (let c = 0; c < rows.length; c++) {
        for (const conn of rows[c].connections) {
          if (conn.is_dangling) continue;
          const t = c + conn.to_row_offset;
          if (t < winStart || t >= winEnd) continue; // parent not rendered here

          const whole = edgeDrawnWhole(
            passes,
            laneX(rows[c].lane),
            yOf(c, 0), // content-space y (offset applied per pass)
            laneX(conn.to_lane),
            yOf(t, 0),
          );
          expect(
            whole,
            `edge c${c}->c${t} (span ${conn.to_row_offset}, window [${winStart},${winEnd})) did not draw whole`,
          ).toBe(true);
        }
      }
    }
  });

  it("ownership is exclusive: strips never bake a long edge the overlay also owns", () => {
    const renderer = new GraphRenderer();
    const rows = boundaryHistory();
    const { ctx, paths } = pathRecordingContext();

    // Strip-only pass over a window containing a span-777 edge's parent.
    const winStart = 700;
    renderer.render(ctx, rows, winStart - LOOKBACK_ROWS, winStart + 20, winStart * ROW_HEIGHT, undefined, {
      viewportHeight: 720,
      skipLongConnectors: true,
    });

    for (const p of paths.filter((q) => q.points.length >= 2)) {
      const span = Math.abs(p.points[p.points.length - 1].y - p.points[0].y) / ROW_HEIGHT;
      expect(span, "a >LOOKBACK connector leaked into the strip layer").toBeLessThanOrEqual(
        LOOKBACK_ROWS + 1,
      );
    }
  });

  it("bypass path (single full render) also leaves no gap at any window", () => {
    const renderer = new GraphRenderer();
    const rows = boundaryHistory(400);
    for (let winStart = 0; winStart < 380; winStart += 7) {
      const { ctx, paths } = pathRecordingContext();
      renderer.render(ctx, rows, winStart, winStart + 20, winStart * ROW_HEIGHT, undefined, {
        viewportHeight: 720,
      });
      const usable = paths.filter((p) => p.points.length >= 2);
      for (let c = 0; c < rows.length; c++) {
        for (const conn of rows[c].connections) {
          if (conn.is_dangling) continue;
          const t = c + conn.to_row_offset;
          if (t < winStart || t >= winStart + 20) continue;
          const xFrom = laneX(rows[c].lane);
          const xTo = laneX(conn.to_lane);
          const yFrom = yOf(c, winStart * ROW_HEIGHT);
          const yTo = yOf(t, winStart * ROW_HEIGHT);
          const whole = usable.some(
            (p) =>
              closeEnough(p.points[0].x, xFrom) &&
              closeEnough(p.points[0].y, yFrom) &&
              closeEnough(p.points[p.points.length - 1].x, xTo) &&
              closeEnough(p.points[p.points.length - 1].y, yTo),
          );
          expect(whole, `bypass dropped c${c}->c${t} at window ${winStart}`).toBe(true);
        }
      }
    }
  });
});
