import { describe, it, expect, vi } from "vitest";
import {
  GraphRenderer,
  themeFromCss,
  DEFAULT_THEME,
  type VisualCommitRow,
  type LaneConnection,
} from "../GraphRenderer";

/**
 * A context that records what was drawn, so the shape of the drawing can be
 * asserted rather than merely the fact that something was drawn.
 */
function recordingContext() {
  const calls: Array<{ op: string; args: number[] }> = [];
  const record = (op: string) => (...args: number[]) => calls.push({ op, args });
  const ctx = {
    save: vi.fn(),
    restore: vi.fn(),
    beginPath: vi.fn(),
    closePath: vi.fn(),
    moveTo: vi.fn(record("moveTo")),
    lineTo: vi.fn(record("lineTo")),
    bezierCurveTo: vi.fn(record("bezierCurveTo")),
    arc: vi.fn(record("arc")),
    fill: vi.fn(),
    stroke: vi.fn(),
    globalAlpha: 1,
    imageSmoothingEnabled: true,
    lineWidth: 2,
    lineCap: "round",
    lineJoin: "round",
    strokeStyle: "",
    fillStyle: "",
    canvas: { height: 600, width: 240 } as HTMLCanvasElement,
  };
  return { ctx: ctx as unknown as CanvasRenderingContext2D, calls, raw: ctx };
}

/**
 * A context whose only extra sense is the alpha each stroke ran at — the
 * fingerprint of dangling-parent stubs, the one translucent mark in the graph.
 */
function strokeAlphaContext(entryAlpha = 1) {
  const alphasAtStroke: number[] = [];
  const ctx = {
    save: vi.fn(),
    restore: vi.fn(() => {
      ctx.globalAlpha = entryAlpha;
    }),
    beginPath: vi.fn(),
    moveTo: vi.fn(),
    lineTo: vi.fn(),
    bezierCurveTo: vi.fn(),
    arc: vi.fn(),
    fill: vi.fn(),
    stroke: () => alphasAtStroke.push(ctx.globalAlpha),
    globalAlpha: entryAlpha,
    imageSmoothingEnabled: true,
    lineWidth: 2,
    lineCap: "round",
    lineJoin: "round",
    strokeStyle: "",
    fillStyle: "",
    canvas: { height: 600, width: 240 } as HTMLCanvasElement,
  };
  return { ctx: ctx as unknown as CanvasRenderingContext2D, alphasAtStroke, raw: ctx };
}

function row(overrides: Partial<VisualCommitRow> & { id: string }): VisualCommitRow {
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

function connection(overrides: Partial<LaneConnection>): LaneConnection {
  return {
    from_lane: 0,
    to_lane: 0,
    to_row_offset: 1,
    is_merge: false,
    color_index: 0,
    ...overrides,
  };
}

describe("graph smoothing", () => {
  it("turns a merged branch away just below the merge commit", () => {
    const renderer = new GraphRenderer();
    const { ctx, calls } = recordingContext();
    const rows = [
      row({
        id: "merge",
        is_merge: true,
        connections: [connection({ to_lane: 1, to_row_offset: 4, is_merge: true, color_index: 1 })],
      }),
      row({ id: "a" }),
      row({ id: "b" }),
      row({ id: "c" }),
      row({ id: "target", lane: 1 }),
    ];

    renderer.render(ctx, rows, 0, 5, 0);

    const curve = calls.find((c) => c.op === "bezierCurveTo");
    expect(curve).toBeDefined();
    const { rowHeight } = renderer.getConfig();
    const mergeY = renderer.getRowY(0, 0);
    const targetY = renderer.getRowY(4, 0);
    const curveEndY = curve!.args[5];
    // The turn happens near the merge commit, not halfway down the span, and
    // the straight run to the parent is what follows it.
    expect(curveEndY).toBeGreaterThan(mergeY);
    expect(curveEndY).toBeLessThan(mergeY + rowHeight);
    expect(curveEndY).toBeLessThan((mergeY + targetY) / 2);
    expect(calls.some((c) => c.op === "lineTo" && c.args[1] === targetY)).toBe(true);
  });

  it("keeps a closing lane straight until it arrives at its parent", () => {
    const renderer = new GraphRenderer();
    const { ctx, calls } = recordingContext();
    const rows = [
      row({
        id: "tip",
        lane: 1,
        connections: [connection({ from_lane: 1, to_lane: 0, to_row_offset: 3 })],
      }),
      row({ id: "x" }),
      row({ id: "y" }),
      row({ id: "parent" }),
    ];

    renderer.render(ctx, rows, 0, 4, 0);

    const curve = calls.find((c) => c.op === "bezierCurveTo");
    expect(curve).toBeDefined();
    const parentY = renderer.getRowY(3, 0);
    const tipY = renderer.getRowY(0, 0);
    // The curve ends exactly on the parent node, and it starts in the lower
    // half of the span: the lane runs straight down before it turns.
    expect(curve!.args[5]).toBeCloseTo(parentY, 5);
    expect(curve!.args[1]).toBeGreaterThan((tipY + parentY) / 2);
  });

  it("draws a fading stub for a parent outside the loaded window", () => {
    const renderer = new GraphRenderer();
    const { ctx, calls, raw } = recordingContext();
    const rows = [
      row({ id: "tip", connections: [connection({ is_dangling: true })] }),
      row({ id: "unrelated" }),
    ];

    // Stubs live in the overlay pass (drawDanglingStubs), not in render():
    // baked into strip tiles they would clip mid-fade at strip seams.
    renderer.drawDanglingStubs(ctx, rows, 0, 2, 0);

    const tipY = renderer.getRowY(0, 0);
    const nextRowY = renderer.getRowY(1, 0);
    const stubEnds = calls
      .filter((c) => c.op === "lineTo")
      .map((c) => c.args[1])
      .filter((y) => y > tipY);
    expect(stubEnds.length).toBeGreaterThan(0);
    // Nothing is drawn down to the unrelated commit on the next row.
    expect(Math.max(...stubEnds)).toBeLessThan(nextRowY);
    // Alpha is restored, or every later mark would inherit the fade.
    expect(raw.globalAlpha).toBe(1);
  });

  it("keeps render() itself free of stub ops so cached strips cannot pick them up", () => {
    const renderer = new GraphRenderer();
    const { ctx, alphasAtStroke } = strokeAlphaContext();
    const rows = [
      row({ id: "tip", connections: [connection({ is_dangling: true })] }),
      row({ id: "unrelated" }),
    ];

    renderer.render(ctx, rows, 0, 2, 0);

    // The strip painter calls render(); if render() ever drew stubs again,
    // translucent strokes would reappear inside cached tiles.
    expect(alphasAtStroke.length).toBeGreaterThan(0);
    expect(alphasAtStroke.every((a) => a === 1)).toBe(true);
  });

  it("merges a lane's pass-through rows into one stroked run", () => {
    const renderer = new GraphRenderer();
    const { ctx, calls } = recordingContext();
    const rows = [
      row({ id: "a", active_lanes: [0, 1], active_lane_colors: [0, 1] }),
      row({ id: "b", active_lanes: [0, 1], active_lane_colors: [0, 1] }),
      row({ id: "c", active_lanes: [0, 1], active_lane_colors: [0, 1] }),
    ];

    renderer.render(ctx, rows, 0, 3, 0);

    const laneX = renderer.getLaneX(1);
    const passThroughMoves = calls.filter((c) => c.op === "moveTo" && c.args[0] === laneX);
    // Three rows of pass-through lane, one path.
    expect(passThroughMoves).toHaveLength(1);
    const { rowHeight } = renderer.getConfig();
    const runEnd = calls.find((c) => c.op === "lineTo" && c.args[0] === laneX);
    expect(runEnd!.args[1] - passThroughMoves[0].args[1]).toBeCloseTo(rowHeight * 3, 5);
  });

  it("cuts the node hole in the theme's background colour, not a fixed dark", () => {
    const renderer = new GraphRenderer();
    const { ctx, raw } = recordingContext();
    const fills: string[] = [];
    Object.defineProperty(raw, "fillStyle", {
      get: () => fills[fills.length - 1] ?? "",
      set: (value: string) => fills.push(value),
    });

    renderer.render(ctx, [row({ id: "only" })], 0, 1, 0, undefined, {
      theme: { ...DEFAULT_THEME, background: "#ffffff" },
    });

    expect(fills).toContain("#ffffff");
    expect(fills).not.toContain(DEFAULT_THEME.background);
  });

  it("falls back to the default theme when no stylesheet is readable", () => {
    const theme = themeFromCss(null);
    expect(theme.background).toBeTruthy();
    expect(theme.selection).toBeTruthy();
  });

  it("sizes the gutter to the deepest lane in the history", () => {
    const renderer = new GraphRenderer();
    const narrow = renderer.measureWidth([row({ id: "a" })]);
    const wide = renderer.measureWidth([
      row({ id: "a", active_lanes: [0, 7], active_lane_colors: [0, 7] }),
    ]);
    expect(wide).toBeGreaterThan(narrow);
  });
});
