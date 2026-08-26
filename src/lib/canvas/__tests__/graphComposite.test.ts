import { describe, it, expect, vi } from "vitest";
import { paintGraphFrame, type GraphFrameRequest } from "../graphComposite";
import { GraphRenderer, type VisualCommitRow } from "../GraphRenderer";
import {
  createGraphStaticCache,
  type CachedSurface,
  type StripPaintRequest,
  type SurfaceFactory,
} from "../graphCache";

/**
 * A fully recording target context: every op is logged in call order so the
 * frame's layering contract can be asserted as a sequence.
 */
function frameRecorder() {
  const ops: Array<{ op: string; alpha: number }> = [];
  const ctx: Record<string, unknown> = {
    save: vi.fn(),
    restore: vi.fn(),
    beginPath: vi.fn(),
    moveTo: vi.fn(),
    lineTo: vi.fn(),
    bezierCurveTo: vi.fn(() => ops.push({ op: "bezierCurveTo", alpha: ctx.globalAlpha as number })),
    arc: vi.fn(() => ops.push({ op: "arc", alpha: ctx.globalAlpha as number })),
    fill: vi.fn(() => ops.push({ op: "fill", alpha: ctx.globalAlpha as number })),
    stroke: vi.fn(() => ops.push({ op: "stroke", alpha: ctx.globalAlpha as number })),
    fillRect: vi.fn(() => ops.push({ op: "fillRect", alpha: ctx.globalAlpha as number })),
    drawImage: vi.fn(() => ops.push({ op: "drawImage", alpha: ctx.globalAlpha as number })),
    setTransform: vi.fn(),
    globalAlpha: 1,
    imageSmoothingEnabled: true,
    lineWidth: 2,
    lineCap: "round",
    lineJoin: "round",
    strokeStyle: "",
    fillStyle: "",
    canvas: { width: 220, height: 120 },
  };
  return { ops, ctx: ctx as unknown as CanvasRenderingContext2D };
}

function fakeSurfaceFactory(): SurfaceFactory {
  return (cssWidth, cssHeight, dpr) => {
    const canvas = {
      width: Math.max(1, Math.round(cssWidth * dpr)),
      height: Math.max(1, Math.round(cssHeight * dpr)),
    } as HTMLCanvasElement;
    const surface: CachedSurface = {
      canvas,
      ctx: {
        setTransform: vi.fn(),
        fillRect: vi.fn(),
        save: vi.fn(),
        restore: vi.fn(),
        beginPath: vi.fn(),
        moveTo: vi.fn(),
        lineTo: vi.fn(),
        bezierCurveTo: vi.fn(),
        arc: vi.fn(),
        fill: vi.fn(),
        stroke: vi.fn(),
        lineWidth: 2,
        lineCap: "round",
        lineJoin: "round",
        imageSmoothingEnabled: true,
        globalAlpha: 1,
        strokeStyle: "",
        fillStyle: "",
      } as unknown as CanvasRenderingContext2D,
    };
    return surface;
  };
}

function row(id: string, overrides: Partial<VisualCommitRow> = {}): VisualCommitRow {
  return {
    id,
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

function frameRequest(rows: VisualCommitRow[], overrides: Partial<GraphFrameRequest> = {}): GraphFrameRequest {
  return {
    rows,
    dataVersion: 1,
    widthCss: 220,
    heightCss: 120,
    dpr: 1,
    densitySignature: "spacious",
    theme: {
      background: "#0d1117",
      nodeStroke: "#161b22",
      selection: "#58a6ff",
      head: "#f0f6fc",
      muted: "#8b949e",
    },
    scrollTop: 0,
    startIndex: 0,
    endIndex: rows.length,
    selectedCommitId: null,
    headCommitId: null,
    hoveredCommitId: null,
    hoverStrength: 1,
    selectionStrength: 1,
    ...overrides,
  };
}

/** Tip with a dangling parent plus a plain commit to select. */
function stubAndSelectionRows(): VisualCommitRow[] {
  return [
    row("tip", {
      connections: [
        { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 1, is_dangling: true },
      ],
    }),
    row("sel"),
  ];
}

describe("paintGraphFrame layering", () => {
  function paintWithRealStrips(req: GraphFrameRequest) {
    const renderer = new GraphRenderer();
    const painterRequests: StripPaintRequest[] = [];
    // The real painter shape: render() into the strip (stubs are skipped by
    // render() itself now), so what the strips bake is exactly production's.
    const cache = createGraphStaticCache(
      (r) => {
        painterRequests.push(r);
        renderer.render(r.ctx, req.rows, Math.max(0, r.firstRow - 60), r.firstRow + r.rowCount - 1, r.stripTopCss, undefined, { viewportHeight: r.viewportCssHeight }, null);
      },
      fakeSurfaceFactory(),
      { stripCssHeight: 72 }, // two rows per strip at rowHeight 36
    );
    const recorder = frameRecorder();
    const blitted = paintGraphFrame(recorder.ctx, recorder.ctx.canvas as HTMLCanvasElement, renderer, cache, req);
    return { recorder, blitted, painterRequests };
  }

  it("layers background → strip blits → dangling stubs → emphasis rings", () => {
    const rows = stubAndSelectionRows();
    const { recorder, blitted } = paintWithRealStrips(
      frameRequest(rows, { selectedCommitId: "sel" }),
    );

    expect(blitted).toBe(true);
    const kinds = recorder.ops.map((o) => o.op);
    const firstFillRect = kinds.indexOf("fillRect");
    const firstBlit = kinds.indexOf("drawImage");
    const lastBlit = kinds.lastIndexOf("drawImage");
    // Stubs are the only translucent strokes; rings come from the emphasis pass.
    const stubIndexes = recorder.ops
      .map((o, i) => (o.op === "stroke" && o.alpha < 1 ? i : -1))
      .filter((i) => i >= 0);
    const arcIndexes = kinds.map((k, i) => (k === "arc" ? i : -1)).filter((i) => i >= 0);

    expect(firstFillRect).toBe(0); // background first
    expect(firstBlit).toBeGreaterThan(firstFillRect); // then strips
    expect(stubIndexes.length).toBeGreaterThanOrEqual(2); // both fade segments ran
    expect(Math.min(...stubIndexes)).toBeGreaterThan(lastBlit); // stubs after blits
    expect(Math.min(...arcIndexes)).toBeGreaterThan(Math.max(...stubIndexes)); // rings last
  });

  it("bypass path still layers stubs under a full render and reports the miss", () => {
    const rows = [row("only", {
      connections: [
        { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0, is_dangling: true },
      ],
    })];
    const { recorder, blitted } = paintWithRealStrips(frameRequest(rows));

    // One row cannot engage the strip cache (totalRows < 2).
    expect(blitted).toBe(false);
    const stubStrokes = recorder.ops.filter((o) => o.op === "stroke" && o.alpha < 1);
    // Stubs drawn exactly once — not doubled by the full render that follows.
    expect(stubStrokes).toHaveLength(2);
    // Full render painted node bodies (fills) after the stub strokes.
    const lastStubIndex = recorder.ops.reduce(
      (acc, o, i) => (o.op === "stroke" && o.alpha < 1 ? i : acc), -1);
    const bodyFillAfterStub = recorder.ops.some(
      (o, i) => o.op === "fill" && i > lastStubIndex,
    );
    expect(bodyFillAfterStub).toBe(true);
  });

  it("overlay draws long connectors whole when strips covered the frame", () => {
    // A merge edge spanning row 0 -> row 90 (offset 90 > LOOKBACK_ROWS=60):
    // baked into tiles it would lose its middle at every seam, so the frame
    // contract hands it to the live overlay instead.
    const rows: VisualCommitRow[] = Array.from({ length: 100 }, (_, i) =>
      row(`c${i}`),
    );
    rows[0].parent_ids = ["c90"];
    rows[0].connections.push({
      from_lane: 0,
      to_lane: 1,
      to_row_offset: 90,
      is_merge: true,
      color_index: 3,
    });
    const { recorder, blitted } = paintWithRealStrips(
      frameRequest(rows, { startIndex: 85, endIndex: 95, scrollTop: 85 * 36 }),
    );

    expect(blitted).toBe(true);
    const lastBlit = recorder.ops.map((o) => o.op).lastIndexOf("drawImage");
    const firstBezier = recorder.ops.map((o) => o.op).indexOf("bezierCurveTo");
    // The long edge's lower segment arrives AFTER the strip blits — drawn
    // whole by the overlay rather than clipped mid-edge at a seam.
    expect(firstBezier).toBeGreaterThan(lastBlit);
    // And its endpoint node was re-stamped over the stroke (repair pass).
    const firstBezierIndex = firstBezier;
    const repairFill = recorder.ops.some(
      (o, i) => o.op === "fill" && i > firstBezierIndex,
    );
    expect(repairFill).toBe(true);
  });
});
