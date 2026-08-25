import { describe, it, expect, vi } from "vitest";
import {
  DENSITY_CONFIGS,
  GraphRenderer,
  DEFAULT_CONFIG,
  emphasisRingRadius,
  type VisualCommitRow,
} from "../GraphRenderer";
import { getBranchColor, BRANCH_PALETTE } from "../Palette";

function createMockContext(): CanvasRenderingContext2D {
  return {
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
    strokeStyle: "",
    fillStyle: "",
    globalAlpha: 1,
    imageSmoothingEnabled: false,
    canvas: { height: 600, width: 180 } as HTMLCanvasElement,
  } as unknown as CanvasRenderingContext2D;
}

describe("GraphRenderer and Palette", () => {
  it("generates deterministic branch colors from palette", () => {
    expect(getBranchColor(0)).toBe(BRANCH_PALETTE[0]);
    expect(getBranchColor(1)).toBe(BRANCH_PALETTE[1]);
    expect(getBranchColor(BRANCH_PALETTE.length)).toBe(BRANCH_PALETTE[0]); // wraps around
  });

  it("initializes GraphRenderer with default configuration", () => {
    const renderer = new GraphRenderer();
    expect(renderer).toBeDefined();
    expect(DEFAULT_CONFIG.rowHeight).toBe(36);
    expect(DEFAULT_CONFIG.laneWidth).toBe(30);
    expect(DEFAULT_CONFIG.nodeRadius).toBe(5);
    expect(DEFAULT_CONFIG.mergeNodeRadius).toBe(6.5);
    expect(DEFAULT_CONFIG.originX).toBe(20);
  });

  it("supports switching density modes between spacious and compact", () => {
    const renderer = new GraphRenderer();
    expect(renderer.getConfig().rowHeight).toBe(36);
    expect(renderer.getConfig().laneWidth).toBe(30);

    renderer.setDensity("compact");
    expect(renderer.getConfig().rowHeight).toBe(26);
    expect(renderer.getConfig().laneWidth).toBe(18);
    expect(renderer.getConfig().nodeRadius).toBe(3.5);

    renderer.setDensity("spacious");
    expect(renderer.getConfig().rowHeight).toBe(36);
    expect(renderer.getConfig().laneWidth).toBe(30);
    expect(renderer.getConfig().nodeRadius).toBe(5);
  });

  it("leaves a full node-width gap between adjacent branches in spacious mode", () => {
    const spacious = DENSITY_CONFIGS.spacious;
    const strokedMergeNodeDiameter = spacious.mergeNodeRadius * 2 + spacious.lineWidth;
    const clearGap = spacious.laneWidth - strokedMergeNodeDiameter;

    expect(clearGap).toBeGreaterThanOrEqual(spacious.mergeNodeRadius * 2);
  });

  it("handles custom renderer configuration overrides", () => {
    const custom = new GraphRenderer({ rowHeight: 40, laneWidth: 28 });
    expect(custom).toBeDefined();
    expect(custom.getConfig().rowHeight).toBe(40);
    expect(custom.getConfig().laneWidth).toBe(28);
  });


  it("calculates getRowY correctly for absolute and relative scrolls", () => {
    const renderer = new GraphRenderer({ rowHeight: 36 });
    // Row 0 at scrollTop = 0 -> center is 18
    expect(renderer.getRowY(0, 0, 0, true)).toBe(18);
    // Row 1 at scrollTop = 36 -> center is 36 + 18 - 36 = 18
    expect(renderer.getRowY(1, 1, 36, true)).toBe(18);
    // Relative scroll calculation
    expect(renderer.getRowY(2, 0, 10, false)).toBe(2 * 36 + 18 - 10);
  });

  it("calculates getLaneX correctly", () => {
    const renderer = new GraphRenderer({ originX: 20, laneWidth: 26 });
    expect(renderer.getLaneX(0)).toBe(20);
    expect(renderer.getLaneX(1)).toBe(46);
    expect(renderer.getLaneX(3)).toBe(20 + 3 * 26);
  });

  it("identifies commit node at point correctly", () => {
    const renderer = new GraphRenderer({ rowHeight: 36, laneWidth: 26, originX: 20 });
    const rows: VisualCommitRow[] = [
      {
        id: "c1",
        parent_ids: [],
        summary: "Commit 1",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1000,
        lane: 0,
        color_index: 0,
        active_lanes: [0],
        active_lane_colors: [0],
        connections: [],
        is_merge: false,
        is_root: true,
      },
    ];

    // Node 0 is at x: 20, y: 18
    const hit = renderer.getCommitAtPoint(20, 18, rows, 0, 1, 0);
    expect(hit?.id).toBe("c1");

    // Miss point far away
    const miss = renderer.getCommitAtPoint(200, 300, rows, 0, 1, 0);
    expect(miss).toBeNull();
  });


  it("renders linear commit history with straight vertical lines and nodes", () => {
    const renderer = new GraphRenderer();
    const ctx = createMockContext();

    const linearRows: VisualCommitRow[] = [
      {
        id: "c2",
        parent_ids: ["c1"],
        summary: "Commit 2",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1000,
        lane: 0,
        color_index: 0,
        active_lanes: [0],
        active_lane_colors: [0],
        connections: [
          { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 },
        ],
        is_merge: false,
        is_root: false,
      },
      {
        id: "c1",
        parent_ids: [],
        summary: "Commit 1",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 900,
        lane: 0,
        color_index: 0,
        active_lanes: [0],
        active_lane_colors: [0],
        connections: [],
        is_merge: false,
        is_root: true,
      },
    ];

    renderer.render(ctx, linearRows, 0, 2, 0);

    expect(ctx.save).toHaveBeenCalled();
    expect(ctx.restore).toHaveBeenCalled();
    expect(ctx.moveTo).toHaveBeenCalled();
    expect(ctx.lineTo).toHaveBeenCalled();
    expect(ctx.arc).toHaveBeenCalled();
    expect(ctx.fill).toHaveBeenCalled();
    expect(ctx.stroke).toHaveBeenCalled();
  });

  it("renders branching curves with cubic bezier splines", () => {
    const renderer = new GraphRenderer();
    const ctx = createMockContext();

    const branchRows: VisualCommitRow[] = [
      {
        id: "merge",
        parent_ids: ["b1", "b2"],
        summary: "Merge commit",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1200,
        lane: 0,
        color_index: 0,
        active_lanes: [0],
        active_lane_colors: [0],
        connections: [
          { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 },
          { from_lane: 0, to_lane: 1, to_row_offset: 2, is_merge: true, color_index: 1 },
        ],
        is_merge: true,
        is_root: false,
      },
      {
        id: "b1",
        parent_ids: ["root"],
        summary: "Branch 1",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1100,
        lane: 0,
        color_index: 0,
        active_lanes: [0, 1],
        active_lane_colors: [0, 1],
        connections: [
          { from_lane: 0, to_lane: 0, to_row_offset: 2, is_merge: false, color_index: 0 },
        ],
        is_merge: false,
        is_root: false,
      },
      {
        id: "b2",
        parent_ids: ["root"],
        summary: "Branch 2",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1050,
        lane: 1,
        color_index: 1,
        active_lanes: [0, 1],
        active_lane_colors: [0, 1],
        connections: [
          { from_lane: 1, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 1 },
        ],
        is_merge: false,
        is_root: false,
      },
      {
        id: "root",
        parent_ids: [],
        summary: "Root",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1000,
        lane: 0,
        color_index: 0,
        active_lanes: [0],
        active_lane_colors: [0],
        connections: [],
        is_merge: false,
        is_root: true,
      },
    ];

    renderer.render(ctx, branchRows, 0, 4, 0, "merge");

    // Bézier curves should be used for cross-lane connections
    expect(ctx.bezierCurveTo).toHaveBeenCalled();
  });

  it("handles empty rows gracefully", () => {
    const renderer = new GraphRenderer();
    const ctx = createMockContext();
    expect(() => renderer.render(ctx, [], 0, 0, 0)).not.toThrow();
  });

  it("renders node selection ring when selectedId matches", () => {
    const renderer = new GraphRenderer();
    const ctx = createMockContext();

    const rows: VisualCommitRow[] = [
      {
        id: "commit_selected",
        parent_ids: [],
        summary: "Selected Commit",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1000,
        lane: 0,
        color_index: 2,
        active_lanes: [0],
        active_lane_colors: [2],
        connections: [],
        is_merge: false,
        is_root: true,
      },
    ];

    renderer.render(ctx, rows, 0, 1, 0, "commit_selected");
    expect(ctx.arc).toHaveBeenCalled();
    expect(ctx.stroke).toHaveBeenCalled();
  });

  it("renders pass-through vertical tracks on intermediate rows", () => {
    const renderer = new GraphRenderer();
    const ctx = createMockContext();

    const rows: VisualCommitRow[] = [
      {
        id: "c_main",
        parent_ids: ["c_root"],
        summary: "Main commit",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1000,
        lane: 0,
        color_index: 0,
        // Lane 1 is passing through from an in-flight branch
        active_lanes: [0, 1],
        active_lane_colors: [0, 1],
        connections: [
          { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 },
        ],
        is_merge: false,
        is_root: false,
      },
    ];

    renderer.render(ctx, rows, 0, 1, 0);
    expect(ctx.lineTo).toHaveBeenCalled();
  });

  it("matches the historical selection ring radius at full strength", () => {
    expect(emphasisRingRadius(5, 1)).toBe(8.5);
    expect(emphasisRingRadius(5, 0)).toBe(7);
    expect(emphasisRingRadius(5, 0.5)).toBe(7.75);
  });

  it("skips the hover ring when hoverStrength is 0", () => {
    const renderer = new GraphRenderer();
    const row: VisualCommitRow = {
      id: "c1",
      parent_ids: [],
      summary: "Commit 1",
      author_name: "Dev",
      author_email: "dev@example.com",
      timestamp: 1000,
      lane: 0,
      color_index: 0,
      active_lanes: [0],
      active_lane_colors: [0],
      connections: [],
      is_merge: false,
      is_root: false,
    };

    const off = createMockContext();
    renderer.render(off, [row], 0, 1, 0, undefined, {
      hoveredCommitId: "c1",
      hoverStrength: 0,
    });
    const arcsOff = (off.arc as ReturnType<typeof vi.fn>).mock.calls.length;

    const on = createMockContext();
    renderer.render(on, [row], 0, 1, 0, undefined, {
      hoveredCommitId: "c1",
      hoverStrength: 1,
    });
    const arcsOn = (on.arc as ReturnType<typeof vi.fn>).mock.calls.length;
    expect(arcsOn).toBeGreaterThan(arcsOff);
  });

  it("fades the hover ring with hoverStrength and restores globalAlpha", () => {
    const renderer = new GraphRenderer();
    const ctx = createMockContext();
    const alphas: number[] = [];
    ctx.stroke = vi.fn(() => {
      alphas.push(ctx.globalAlpha);
    });

    renderer.render(ctx, [
      {
        id: "c1",
        parent_ids: [],
        summary: "Commit 1",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1000,
        lane: 0,
        color_index: 0,
        active_lanes: [0],
        active_lane_colors: [0],
        connections: [],
        is_merge: false,
        is_root: false,
      },
    ], 0, 1, 0, undefined, { hoveredCommitId: "c1", hoverStrength: 0.4 });

    expect(alphas).toContain(0.4);
    expect(ctx.globalAlpha).toBe(1);
  });

  it("skips the selection ring when selectionStrength is 0", () => {
    const renderer = new GraphRenderer();
    const row: VisualCommitRow = {
      id: "commit_selected",
      parent_ids: [],
      summary: "Selected Commit",
      author_name: "Dev",
      author_email: "dev@example.com",
      timestamp: 1000,
      lane: 0,
      color_index: 2,
      active_lanes: [0],
      active_lane_colors: [2],
      connections: [],
      is_merge: false,
      is_root: true,
    };

    const off = createMockContext();
    renderer.render(off, [row], 0, 1, 0, "commit_selected", { selectionStrength: 0 });
    const arcsOff = (off.arc as ReturnType<typeof vi.fn>).mock.calls.length;

    const on = createMockContext();
    renderer.render(on, [row], 0, 1, 0, "commit_selected", { selectionStrength: 1 });
    const arcsOn = (on.arc as ReturnType<typeof vi.fn>).mock.calls.length;
    expect(arcsOn).toBeGreaterThan(arcsOff);
  });

  it("emphasis-only mode draws rings but no cutout or node bodies", () => {
    const renderer = new GraphRenderer();
    const rows: VisualCommitRow[] = [
      {
        id: "sel",
        parent_ids: [],
        summary: "s",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1,
        lane: 0,
        color_index: 0,
        active_lanes: [0],
        active_lane_colors: [0],
        connections: [
          { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 },
        ],
        is_merge: false,
        is_root: false,
      },
    ];

    const full = createMockContext();
    renderer.render(full, rows, 0, 1, 0, "sel");
    expect(full.fill).toHaveBeenCalled();

    // Same graph with an unrelated selection: nothing emphasized, nothing drawn.
    const quiet = createMockContext();
    renderer.render(quiet, rows, 0, 1, 0, "other", { emphasisOnly: true });
    expect(quiet.arc).not.toHaveBeenCalled();
    expect(quiet.stroke).not.toHaveBeenCalled();

    // Emphasis mode rings the selected node but never paints a body/cutout.
    const overlay = createMockContext();
    renderer.render(overlay, rows, 0, 1, 0, "sel", {
      headCommitId: "sel",
      emphasisOnly: true,
    });
    expect(overlay.arc).toHaveBeenCalled();
    expect(overlay.stroke).toHaveBeenCalled();
    expect(overlay.fill).not.toHaveBeenCalled();
    expect(overlay.bezierCurveTo).not.toHaveBeenCalled(); // no connectors either
  });

  it("reuses module scratch state without leaking between consecutive renders", () => {
    const renderer = new GraphRenderer();
    const rows: VisualCommitRow[] = [
      {
        id: "a",
        parent_ids: ["b"],
        summary: "s",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 1,
        lane: 0,
        color_index: 0,
        active_lanes: [0, 1, 2],
        active_lane_colors: [0, 1, 2],
        connections: [
          { from_lane: 0, to_lane: 1, to_row_offset: 1, is_merge: false, color_index: 1 },
        ],
        is_merge: false,
        is_root: false,
      },
      {
        id: "b",
        parent_ids: [],
        summary: "s",
        author_name: "Dev",
        author_email: "dev@example.com",
        timestamp: 0,
        lane: 1,
        color_index: 1,
        active_lanes: [0, 1],
        active_lane_colors: [0, 1],
        connections: [],
        is_merge: false,
        is_root: false,
      },
    ];

    const trace = () => {
      const ctx = createRecordingContextForScratch();
      renderer.render(ctx.ctx, rows, 0, 2, 0);
      return JSON.stringify(ctx.calls);
    };

    // Interleave a second renderer instance: shared scratch must stay clean.
    const first = trace();
    void new GraphRenderer({ rowHeight: 40 }).render(
      createRecordingContextForScratch().ctx,
      rows,
      0,
      2,
      0
    );
    expect(trace()).toBe(first);
  });
});

describe("drawDanglingStubs overlay API", () => {
  const DANGLING = { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 3, is_dangling: true };
  const PLAIN = { from_lane: 0, to_lane: 0, to_row_offset: 1, is_merge: false, color_index: 0 };

  function danglingRow(id: string, connections = [DANGLING]): VisualCommitRow {
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
      connections,
      is_merge: false,
      is_root: false,
    };
  }

  /** Recording context that also logs the alpha each stroke ran at. */
  function stubContext(entryAlpha = 1) {
    const moves: Array<[number, number]> = [];
    const lines: Array<[number, number]> = [];
    const alphasAtStroke: number[] = [];
    const ctx = {
      save: vi.fn(),
      restore: vi.fn(() => {
        ctx.globalAlpha = entryAlpha;
      }),
      beginPath: vi.fn(),
      moveTo: vi.fn((x: number, y: number) => moves.push([x, y])),
      lineTo: vi.fn((x: number, y: number) => lines.push([x, y])),
      stroke: () => alphasAtStroke.push(ctx.globalAlpha),
      globalAlpha: entryAlpha,
      lineWidth: 2,
      lineCap: "round" as CanvasLineCap,
      lineJoin: "round" as CanvasLineJoin,
      strokeStyle: "",
      fillStyle: "",
      canvas: { height: 600, width: 180 } as HTMLCanvasElement,
    };
    return { ctx: ctx as unknown as CanvasRenderingContext2D, moves, lines, alphasAtStroke, raw: ctx };
  }

  it("fades in two segments with the historical geometry and alpha math", () => {
    const renderer = new GraphRenderer(); // spacious: rowHeight 36
    const { ctx, moves, lines, alphasAtStroke } = stubContext();
    const rows = [danglingRow("tip")];

    renderer.drawDanglingStubs(ctx, rows, 0, 1, 0);

    const tipY = renderer.getRowY(0, 0, 0, true);
    const x = renderer.getLaneX(0);
    expect(moves[0]).toEqual([x, tipY]);
    expect(lines[0][1]).toBeCloseTo(tipY + 36 * 0.62 * 0.6, 5);
    expect(lines[1][1]).toBeCloseTo(tipY + 36 * 0.62, 5);
    expect(alphasAtStroke).toEqual([0.55, 0.22]);
  });
  it("restores the caller's alpha, scaling the fade by whatever was set", () => {
    const renderer = new GraphRenderer();
    const { ctx, alphasAtStroke, raw } = stubContext(0.5);
    renderer.drawDanglingStubs(ctx, [danglingRow("tip")], 0, 1, 0);
    expect(alphasAtStroke).toEqual([0.5 * 0.55, 0.5 * 0.22]);
    expect(raw.globalAlpha).toBe(0.5);
  });

  it("draws only rows inside [startIndex, endIndex)", () => {
    const renderer = new GraphRenderer();
    const inside = stubContext();
    renderer.drawDanglingStubs(inside.ctx, [danglingRow("a"), danglingRow("b")], 1, 2, 0);
    // Only row 1's node lane saw a stub.
    expect(inside.alphasAtStroke).toEqual([0.55, 0.22]);

    const outside = stubContext();
    renderer.drawDanglingStubs(outside.ctx, [danglingRow("a")], 1, 2, 0);
    expect(outside.alphasAtStroke).toHaveLength(0);
  });

  it("culls stubs whose commit sits outside the viewport band", () => {
    const renderer = new GraphRenderer({ rowHeight: 36 });
    const rows = [danglingRow("near"), danglingRow("far")];
    const { ctx, alphasAtStroke } = stubContext();

    // Cull band is [-rowHeight, viewport + rowHeight]: with a 10px viewport,
    // row 0 (centre 18) draws and row 1 (centre 54) is culled.
    renderer.drawDanglingStubs(ctx, rows, 0, 2, 0, 10);

    expect(alphasAtStroke).toEqual([0.55, 0.22]);
  });

  it("ignores plain connections entirely — no stub without is_dangling", () => {
    const renderer = new GraphRenderer();
    const { ctx, alphasAtStroke } = stubContext();
    renderer.drawDanglingStubs(ctx, [danglingRow("mid", [PLAIN])], 0, 1, 0);
    expect(alphasAtStroke).toHaveLength(0);
  });

  it("handles empty and null-ish row inputs without throwing", () => {
    const renderer = new GraphRenderer();
    const { ctx } = stubContext();
    expect(() => renderer.drawDanglingStubs(ctx, [], 0, 0, 0)).not.toThrow();
  });
});

/** Records every path op so two renders can be compared byte-for-byte. */
function createRecordingContextForScratch() {
  const calls: Array<{ op: string; args: unknown[] }> = [];
  const record = (op: string) => (...args: unknown[]) => calls.push({ op, args });
  return {
    calls,
    ctx: {
      save: vi.fn(),
      restore: vi.fn(),
      beginPath: vi.fn(),
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
      canvas: { height: 600, width: 180 } as HTMLCanvasElement,
    } as unknown as CanvasRenderingContext2D,
  };
}
