import { describe, expect, it, vi } from "vitest";
import { GraphRenderer, LOOKBACK_ROWS, type VisualCommitRow } from "../GraphRenderer";

/**
 * Stress contract for the connector ownership split.
 *
 * Per-frame cost must scale with the VIEWPORT (rows on screen), never with
 * loaded history — and an edge of ANY span must draw whole. The old fixed
 * scan caps (60-row lookback, then a 480-row "tail" band) silently dropped
 * the middle of longer edges as floating fragments at strip seams.
 */

function countingContext(): {
  ctx: CanvasRenderingContext2D;
  counts: Record<string, number>;
} {
  const counts: Record<string, number> = {};
  const tally = (op: string) => () => void (counts[op] = (counts[op] ?? 0) + 1);
  const ctx = {
    save: vi.fn(),
    restore: vi.fn(),
    beginPath: tally("beginPath"),
    moveTo: vi.fn(),
    lineTo: vi.fn(),
    bezierCurveTo: tally("bezier"),
    arc: vi.fn(),
    fill: vi.fn(),
    stroke: tally("stroke"),
    lineWidth: 2,
    lineCap: "round",
    lineJoin: "round",
    globalAlpha: 1,
    imageSmoothingEnabled: true,
    strokeStyle: "",
    fillStyle: "",
    canvas: { height: 720, width: 300 } as HTMLCanvasElement,
  } as unknown as CanvasRenderingContext2D;
  return { ctx, counts };
}

function plainRow(id: string): VisualCommitRow {
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
    connections: [],
    is_merge: false,
    is_root: false,
  };
}

/** n linear rows; row `childIdx` merges a branch tip sitting `span` rows above its parent. */
function historyWithLongEdge(n: number, childIdx: number, span: number) {
  const rows = Array.from({ length: n }, (_, i) => plainRow(`c${i}`));
  const target = childIdx + span;
  rows[childIdx].parent_ids = [`c${target}`];
  rows[childIdx].connections.push({
    from_lane: 0,
    to_lane: 1,
    to_row_offset: span,
    is_merge: true,
    color_index: 3,
  });
  return rows;
}

describe("long-edge rendering under stress", () => {
  it("draws an edge spanning 9,000 rows whole when only its tail is visible", () => {
    const renderer = new GraphRenderer();
    const rows = historyWithLongEdge(10_000, 500, 9_000); // target row 9_500
    const { ctx, counts } = countingContext();

    // Viewport over rows ~9490-9510 at scrollTop 9490*36.
    renderer.render(ctx, rows, 9_490, 9_510, 9_490 * 36);

    expect(counts.bezier).toBe(1); // corner-at-start merge geometry, once
  });

  it("keeps strip painting O(window + lookback) on a 100k-row history", () => {
    const renderer = new GraphRenderer();
    // Dense connectivity: every row connects to the next three (worst
    // realistic short-edge density).
    const rows = Array.from({ length: 100_000 }, (_, i) => {
      const row = plainRow(`c${i}`);
      for (let k = 1; k <= 3 && i + k < 100_000; k++) {
        row.connections.push({
          from_lane: 0,
          to_lane: 0,
          to_row_offset: k,
          is_merge: false,
          color_index: 0,
        });
      }
      return row;
    });
    const { ctx, counts } = countingContext();

    const from = Math.max(0, 50_000 - LOOKBACK_ROWS);
    renderer.render(ctx, rows, from, 50_040, 50_000 * 36, undefined, {
      skipLongConnectors: true,
      viewportHeight: 1440,
    });

    // Connector strokes bounded by lookback+window rows x 3 connections:
    // 100 rows * 3 = 300 max; allow slack but never history-scale (100k).
    expect(counts.stroke!).toBeLessThanOrEqual(320);
    expect(counts.stroke!).toBeGreaterThan(0);
  });

  it("drawLongConnectors enumerates via the index without scanning history", () => {
    const renderer = new GraphRenderer();
    // One mega-edge plus dense short edges elsewhere; overlay must touch
    // only edges landing in the visible window.
    const rows = Array.from({ length: 20_000 }, (_, i) => plainRow(`c${i}`));
    rows[7].connections.push({
      from_lane: 0,
      to_lane: 1,
      to_row_offset: 19_000, // target row 19_007
      is_merge: false,
      color_index: 5,
    });
    const { ctx, counts } = countingContext();

    renderer.drawLongConnectors(ctx, rows, 19_000, 19_020, 19_000 * 36, 720, {});

    // One connector stroke plus node-repair stamps for the visible rows its
    // conservative bounding box touches — bounded by the ~20-row window,
    // NEVER by the 20k-row history.
    const totalOps =
      (counts.stroke ?? 0) + (counts.beginPath ?? 0) + (counts.bezier ?? 0);
    expect(counts.stroke!).toBeGreaterThan(0);
    expect(totalOps).toBeLessThanOrEqual(4 * 21);
  });

  it("survives hostile offsets: self-referencing and past-the-end connections", () => {
    const renderer = new GraphRenderer();
    const rows = Array.from({ length: 6 }, (_, i) => plainRow(`c${i}`));
    rows[2].connections.push(
      {
        from_lane: 0,
        to_lane: 0,
        to_row_offset: 0, // points at itself — malformed
        is_merge: false,
        color_index: 0,
      },
      {
        from_lane: 0,
        to_lane: 0,
        to_row_offset: 99, // past the end
        is_merge: false,
        color_index: 0,
      },
    );
    const { ctx, counts } = countingContext();
    expect(() =>
      renderer.drawLongConnectors(ctx, rows, 0, 6, 0, 720, {}),
    ).not.toThrow();
    expect(counts.stroke ?? 0).toBe(0);
    expect(() => renderer.render(ctx, rows, 0, 6, 0)).not.toThrow();
  });

  it("covers edges landing in the window's bottom fudge band, however far their child sits", () => {
    // The connector loop runs to endIndex + 5 so partially-scrolled bottom
    // rows render; the index query that extends the window upward MUST use
    // the same bound or an edge landing in those extra rows loses its upper
    // segment whenever its child sits beyond the fixed lookback.
    const renderer = new GraphRenderer();
    const rows = historyWithLongEdge(2_000, 10, 900); // child row 10 -> target row 910
    const { ctx, counts } = countingContext();

    // Window [890, 906): the target row 910 sits inside the +5 fudge band.
    renderer.render(ctx, rows, 890, 906, 890 * 36);

    expect(counts.bezier).toBe(1);
  });
});
