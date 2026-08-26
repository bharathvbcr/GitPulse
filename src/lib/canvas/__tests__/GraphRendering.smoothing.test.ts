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
  it("starts a connector on conn.from_lane even when it disagrees with row.lane", () => {
    // Hostile / remapped payloads can split the two. Drawing from row.lane
    // while treating from_lane === to_lane as "vertical" paints a diagonal
    // the solver never emitted — a long horizontal across unrelated columns.
    const renderer = new GraphRenderer();
    const { ctx, calls } = recordingContext();
    const rows = [
      row({
        id: "child",
        lane: 0,
        connections: [connection({ from_lane: 5, to_lane: 5, to_row_offset: 1 })],
      }),
      row({ id: "parent", lane: 5 }),
    ];
    renderer.render(ctx, rows, 0, 2, 0);

    const fromX = renderer.laneXForRow(rows[0], 5);
    const wrongX = renderer.laneXForRow(rows[0], 0);
    const toX = renderer.laneXForRow(rows[1], 5);
    const parentY = renderer.getRowY(1, 0);
    const childY = renderer.getRowY(0, 0);
    const startsOnFromLane = calls.some(
      (c) =>
        (c.op === "moveTo" || c.op === "lineTo") &&
        Math.abs(c.args[0] - fromX) < 0.01 &&
        Math.abs(c.args[1] - childY) < 0.01,
    );
    expect(startsOnFromLane).toBe(true);
    const landsOnParent = calls.some((c) => {
      if (c.op === "bezierCurveTo") {
        return Math.abs(c.args[4] - toX) < 0.01 && Math.abs(c.args[5] - parentY) < 0.01;
      }
      return (
        (c.op === "moveTo" || c.op === "lineTo") &&
        Math.abs(c.args[0] - toX) < 0.01 &&
        Math.abs(c.args[1] - parentY) < 0.01
      );
    });
    expect(landsOnParent).toBe(true);
    const fromWrongLane = calls.some(
      (c) =>
        (c.op === "moveTo" || c.op === "lineTo") &&
        Math.abs(c.args[0] - wrongX) < 0.01 &&
        Math.abs(c.args[1] - childY) < 0.01,
    );
    expect(fromWrongLane).toBe(false);
  });

  it("turns a leftward merged branch away just below the merge commit", () => {
    const renderer = new GraphRenderer();
    const { ctx, calls } = recordingContext();
    const rows = [
      row({
        id: "merge",
        lane: 2,
        is_merge: true,
        connections: [
          connection({ from_lane: 2, to_lane: 0, to_row_offset: 4, is_merge: true, color_index: 1 }),
        ],
      }),
      row({ id: "a", lane: 2 }),
      row({ id: "b", lane: 2 }),
      row({ id: "c", lane: 2 }),
      row({ id: "target", lane: 0 }),
    ];

    renderer.render(ctx, rows, 0, 5, 0);

    const curve = calls.find((c) => c.op === "bezierCurveTo");
    expect(curve).toBeDefined();
    const { rowHeight } = renderer.getConfig();
    const mergeY = renderer.getRowY(0, 0);
    const targetY = renderer.getRowY(4, 0);
    const curveEndY = curve!.args[5];
    expect(curveEndY).toBeGreaterThan(mergeY);
    expect(curveEndY).toBeLessThan(mergeY + rowHeight);
    expect(curveEndY).toBeLessThan((mergeY + targetY) / 2);
    expect(curve!.args[4]).toBeCloseTo(renderer.getLaneX(0), 5);
    expect(calls.some((c) => c.op === "lineTo" && c.args[1] === targetY)).toBe(true);
  });

  it("does not stroke non-forward or NaN connector offsets", () => {
    const renderer = new GraphRenderer();
    const clean = [row({ id: "a" }), row({ id: "b" })];
    const hostile = [
      row({
        id: "a",
        connections: [
          connection({ to_row_offset: 0 }),
          connection({ to_row_offset: -5 }),
          connection({ to_row_offset: Number.NaN }),
        ],
      }),
      row({ id: "b" }),
    ];
    const a = recordingContext();
    const b = recordingContext();
    renderer.render(a.ctx, clean, 0, 2, 0);
    renderer.render(b.ctx, hostile, 0, 2, 0);
    expect(b.calls.filter((c) => c.op === "moveTo" || c.op === "lineTo")).toEqual(
      a.calls.filter((c) => c.op === "moveTo" || c.op === "lineTo"),
    );
  });

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

  it("counts conn.from_lane even when it disagrees with row.lane", () => {
    const renderer = new GraphRenderer();
    const aligned = renderer.measureWidth([
      row({ id: "a", lane: 0, connections: [connection({ from_lane: 0, to_lane: 0 })] }),
    ]);
    const hostile = renderer.measureWidth([
      row({
        id: "a",
        lane: 0,
        active_lanes: [0],
        connections: [connection({ from_lane: 5, to_lane: 0, to_row_offset: 1 })],
      }),
    ]);
    expect(hostile).toBeGreaterThan(aligned);
    expect(hostile).toBe(
      renderer.measureWidth([
        row({ id: "b", lane: 5, active_lanes: [0, 5], active_lane_colors: [0, 5] }),
      ]),
    );
  });

  it("keeps a hole open instead of collapsing the far branch's column", () => {
    // Lanes are stable columns: a branch on lane 8 stays at column 8 even
    // when lanes 1..7 are empty, so the gutter must genuinely reach it. The
    // solver's interval allocation guarantees such holes are transient and
    // total width equals peak occupancy — the renderer must not "help" by
    // repacking, which is what made lanes jog.
    const renderer = new GraphRenderer();
    const gapped = renderer.measureWidth([
      row({ id: "a", lane: 8, active_lanes: [0, 8], active_lane_colors: [0, 8] }),
    ]);
    const dense = renderer.measureWidth([
      row({ id: "b", lane: 1, active_lanes: [0, 1], active_lane_colors: [0, 1] }),
    ]);
    expect(gapped).toBeGreaterThan(dense);
    expect(gapped).toBe(
      renderer.measureWidth([
        row({
          id: "c",
          lane: 8,
          active_lanes: [0, 1, 2, 3, 4, 5, 6, 7, 8],
          active_lane_colors: [0, 1, 2, 3, 4, 5, 6, 7, 8],
        }),
      ]),
    );
  });

  it("keeps a live branch at the same x when a lower neighbour dies", () => {
    const renderer = new GraphRenderer();
    const { ctx, calls } = recordingContext();
    const rows = [
      row({
        id: "a",
        lane: 1,
        active_lanes: [0, 1, 8],
        active_lane_colors: [0, 1, 8],
      }),
      row({
        id: "b",
        lane: 1,
        active_lanes: [1, 8],
        active_lane_colors: [1, 8],
      }),
    ];
    renderer.render(ctx, rows, 0, 2, 0);

    // Lane 8's x is identical on both rows — the death of lane 0 must not
    // slide it. Its pass-through is one unbroken vertical spanning both
    // rows; no stroke ever visits any other x for that lane.
    const stableX = renderer.getLaneX(8);
    expect(renderer.laneXForRow(rows[0], 8)).toBe(stableX);
    expect(renderer.laneXForRow(rows[1], 8)).toBe(stableX);
    const { rowHeight } = renderer.getConfig();
    const track = calls.some(
      (c, i) =>
        c.op === "moveTo" &&
        Math.abs(c.args[0] - stableX) < 0.5 &&
        calls[i + 1]?.op === "lineTo" &&
        Math.abs(calls[i + 1].args[0] - stableX) < 0.5 &&
        Math.abs(calls[i + 1].args[1] - c.args[1] - 2 * rowHeight) < 0.5,
    );
    expect(track, "lane 8 must draw as one full-height vertical at its own column").toBe(true);
  });

  it("draws a distant parent hop to its real column with a single bend", () => {
    const renderer = new GraphRenderer();
    const { ctx, calls } = recordingContext();
    const rows = [
      row({
        id: "child",
        lane: 0,
        connections: [connection({ from_lane: 0, to_lane: 8, to_row_offset: 1 })],
      }),
      row({ id: "parent", lane: 8, active_lanes: [8] }),
    ];
    renderer.render(ctx, rows, 0, 2, 0);

    // The edge starts on the child's own column, turns exactly once just
    // off that column, and finishes with a flat approach that arrives at
    // the parent's real column on the parent's row — never at an invented
    // intermediate position, never smearing the approach across rows.
    const xFrom = renderer.getLaneX(0);
    const xTo = renderer.getLaneX(8);
    const parentY = renderer.getRowY(1, 0);
    const started = calls.some(
      (c) => c.op === "moveTo" && Math.abs(c.args[0] - xFrom) < 0.5,
    );
    const turned = calls.some(
      (c) =>
        c.op === "bezierCurveTo" &&
        Math.abs(c.args[5] - parentY) < 0.5 &&
        c.args[4] > Math.min(xFrom, xTo) &&
        c.args[4] < Math.max(xFrom, xTo),
    );
    const arrived = calls.some(
      (c) =>
        c.op === "lineTo" &&
        Math.abs(c.args[0] - xTo) < 0.5 &&
        Math.abs(c.args[1] - parentY) < 0.5,
    );
    expect(started, "edge must leave from the child's column").toBe(true);
    expect(turned, "edge must turn onto the parent's row with a single corner").toBe(true);
    expect(arrived, "edge must arrive flat at the parent's real column").toBe(true);
  });

  it("reserves a live destination hop's column in the gutter", () => {
    // A live edge genuinely occupies its target column while it descends;
    // clipping it at a packed width was how edges vanished off the right
    // side of the gutter. Dangling destinations still cost nothing (stubs
    // draw on from_lane) — see the dangling gutter test below.
    const renderer = new GraphRenderer();
    const hop = renderer.measureWidth([
      row({
        id: "child",
        lane: 0,
        connections: [connection({ from_lane: 0, to_lane: 8, to_row_offset: 1 })],
      }),
      row({ id: "parent", lane: 8, active_lanes: [8] }),
    ]);
    const linear = renderer.measureWidth([
      row({ id: "a", lane: 0, active_lanes: [0] }),
      row({ id: "b", lane: 0, active_lanes: [0] }),
    ]);
    expect(hop).toBeGreaterThan(linear);
    expect(hop).toBe(
      renderer.measureWidth([row({ id: "c", lane: 8, active_lanes: [8] })]),
    );
  });

  it("peels a merged-in branch once and descends its own stable column", () => {
    const renderer = new GraphRenderer();
    const { ctx, calls } = recordingContext();
    const rows = [
      row({
        id: "merge",
        is_merge: true,
        connections: [connection({ from_lane: 0, to_lane: 8, to_row_offset: 2, is_merge: true })],
      }),
      row({
        id: "mid",
        lane: 0,
        active_lanes: [0, 8],
        active_lane_colors: [0, 8],
      }),
      row({ id: "parent", lane: 8, active_lanes: [8] }),
    ];
    renderer.render(ctx, rows, 0, 3, 0);

    // The peel bends just under the merge commit onto column 8, then runs
    // straight down: the corner lands on x8 within the first row, and the
    // final stroke arrives at (x8, parentY) with no other x in between.
    const x8 = renderer.getLaneX(8);
    const mergeY = renderer.getRowY(0, 0);
    const parentY = renderer.getRowY(2, 0);
    const { rowHeight } = renderer.getConfig();
    const peeled = calls.some(
      (c) =>
        c.op === "bezierCurveTo" &&
        Math.abs(c.args[4] - x8) < 0.5 &&
        c.args[5] > mergeY &&
        c.args[5] <= mergeY + rowHeight,
    );
    const arrived = calls.some(
      (c) =>
        c.op === "lineTo" &&
        Math.abs(c.args[0] - x8) < 0.5 &&
        Math.abs(c.args[1] - parentY) < 0.5,
    );
    expect(peeled, "merge edge must bend onto its column just below the merge").toBe(true);
    expect(arrived, "merge edge must descend to the parent on one column").toBe(true);
  });

  it("does not let a dangling to_lane widen the gutter", () => {
    // Stubs stroke on from_lane. A ghost parent column is not drawn.
    const renderer = new GraphRenderer();
    const stub = renderer.measureWidth([
      row({
        id: "tip",
        lane: 0,
        active_lanes: [0],
        connections: [connection({ from_lane: 0, to_lane: 20, is_dangling: true })],
      }),
    ]);
    const linear = renderer.measureWidth([row({ id: "a", lane: 0, active_lanes: [0] })]);
    expect(stub).toBe(linear);
  });
});
