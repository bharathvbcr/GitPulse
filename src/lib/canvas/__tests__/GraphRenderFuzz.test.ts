import { describe, expect, it } from "vitest";
import { GraphRenderer, type VisualCommitRow } from "../GraphRenderer";
import { authorIdentity } from "../../authors/authorIdentity";
import { makeRecordingCtx } from "./recordingCtx";

/**
 * Fuzz the full render pipeline (lanes → connectors → nodes → avatars) with
 * adversarial row payloads: NaN/negative lanes, self-referencing and
 * negative-offset connections, dangling cycles, empty strings for author
 * fields, and 20k-row histories. The only contract: never throw, always
 * leave the scratch/pool state clean, and produce bounded output.
 *
 * Seeded LCG so any failure reproduces exactly.
 */
function lcg(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 2 ** 32;
  };
}

function hostileRow(id: string, rnd: () => number): VisualCommitRow {
  const pick = <T,>(arr: T[]): T => arr[Math.floor(rnd() * arr.length)];
  return {
    id,
    parent_ids: [],
    summary: "",
    author_name: pick(["", "   ", "\u0000", "🚀🚀", "x".repeat(500)]),
    author_email: pick(["", "a@b.c", "\uD800"]),
    timestamp: pick([0, -5, Number.NaN]),
    lane: pick([0, 3, Number.NaN, -2]),
    color_index: pick([0, 11, 99, -1, Number.POSITIVE_INFINITY]),
    active_lanes: [0, Number.NaN, 7, -1],
    active_lane_colors: [0],
    connections: [
      {
        from_lane: 0,
        to_lane: pick([0, 2, Number.NaN]),
        to_row_offset: pick([1, 0, -3, 100_000]),
        is_merge: rnd() < 0.5,
        color_index: -4,
      },
      {
        from_lane: 0,
        to_lane: 0,
        to_row_offset: 0,
        is_merge: false,
        color_index: 1,
        is_dangling: true,
      },
    ],
    is_merge: rnd() < 0.3,
    is_root: rnd() < 0.2,
  };
}

const THEME = {
  background: "#ffffff",
  nodeStroke: "#dddddd",
  selection: "#0000ff",
  head: "#111111",
  muted: "#888888",
};

describe("render pipeline fuzz", () => {
  it("survives 300 hostile frames without throwing or corrupting subsequent renders", () => {
    const rnd = lcg(2026);
    const renderer = new GraphRenderer({ rowHeight: 24, laneWidth: 16, originX: 12 });

    const cleanRows: VisualCommitRow[] = Array.from({ length: 40 }, (_, i) => ({
      id: `c${i}`,
      parent_ids: [],
      summary: "clean",
      author_name: `Dev ${i % 5}`,
      author_email: `dev${i % 5}@example.com`,
      timestamp: 100,
      lane: i % 4,
      color_index: i % 12,
      active_lanes: [i % 4],
      active_lane_colors: [i % 4],
      connections: [],
      is_merge: false,
      is_root: false,
    }));

    let cleanTraceAfterHostile = "";
    for (let frame = 0; frame < 300; frame++) {
      const rows =
        frame % 3 === 0
          ? cleanRows
          : Array.from({ length: Math.floor(rnd() * 60) + 1 }, (_, i) => hostileRow(`h${frame}-${i}`, rnd));
      const { ctx } = makeRecordingCtx(400);
      expect(() =>
        renderer.render(ctx, rows, 0, rows.length, Math.floor(rnd() * 200), undefined, {
          theme: THEME,
          viewportHeight: 400,
          showAvatars: frame % 2 === 0,
          avatarX: frame % 2 === 0 ? 180 : null,
        }),
      ).not.toThrow();
      if (frame === 299) {
        const traceCtx = makeRecordingCtx(400);
        renderer.render(traceCtx.ctx, cleanRows, 0, cleanRows.length, 0);
        cleanTraceAfterHostile = JSON.stringify(traceCtx.calls);
      }
    }

    // A pristine renderer must produce byte-identical output for the same
    // clean input: hostile frames may not leak state through the pool.
    const pristine = new GraphRenderer({ rowHeight: 24, laneWidth: 16, originX: 12 });
    const pristineCtx = makeRecordingCtx(400);
    pristine.render(pristineCtx.ctx, cleanRows, 0, cleanRows.length, 0);
    expect(cleanTraceAfterHostile).toBe(JSON.stringify(pristineCtx.calls));
  });

  it("keeps avatar identity lookups bounded across a 20k-row history", () => {
    const rows: VisualCommitRow[] = Array.from({ length: 20_000 }, (_, i) => ({
      id: `r${i}`,
      parent_ids: [],
      summary: "",
      author_name: `Author ${i % 50}`,
      author_email: `a${i % 50}@example.com`,
      timestamp: 1,
      lane: i % 8,
      color_index: i % 12,
      active_lanes: [i % 8],
      active_lane_colors: [i % 8],
      connections: [],
      is_merge: false,
      is_root: false,
    }));
    const renderer = new GraphRenderer({ rowHeight: 24 });
    const { ctx, calls } = makeRecordingCtx(600);
    renderer.render(ctx, rows, 9900, 10200, 9900 * 24, undefined, {
      theme: THEME,
      viewportHeight: 600,
      showAvatars: true,
      avatarX: 160,
    });
    // Only the visible window carries labels — not 20k of them.
    const labels = calls.filter((c) => c.op === "fillText");
    expect(labels.length).toBeGreaterThan(0);
    expect(labels.length).toBeLessThan(400);
    // Distinct authors in window resolve to at most 50 identities.
    const texts = new Set(labels.map((l) => l.text));
    expect(texts.size).toBeLessThanOrEqual(50);
  });

  it("hit-testing tolerates hostile coordinates", () => {
    const renderer = new GraphRenderer();
    const rows = [
      {
        ...({
          id: "a",
          parent_ids: [],
          summary: "",
          author_name: "",
          author_email: "",
          timestamp: 0,
          lane: Number.NaN,
          color_index: 0,
          active_lanes: [],
          active_lane_colors: [],
          connections: [],
          is_merge: false,
          is_root: false,
        } as VisualCommitRow),
      },
    ];
    expect(renderer.getCommitAtPoint(Number.NaN, 10, rows, 0, 1, 0)).toBeNull();
    expect(renderer.getCommitAtPoint(Number.POSITIVE_INFINITY, -Infinity, rows, 0, 1, 0, Infinity)).toBeNull();
    expect(renderer.getCommitAtPoint(-1e9, 1e9, [], 0, 0, 0)).toBeNull();
  });

  it("avatar initials stay consistent between canvas and identity module under fuzz", () => {
    const rnd = lcg(31337);
    const rows: VisualCommitRow[] = [];
    const expected: string[] = [];
    for (let i = 0; i < 500; i++) {
      const name = ["Ada Lovelace", "", "Ünal Öztürk", "🚀", "x"][Math.floor(rnd() * 5)];
      const email = [`u${i % 7}@ex.com`, "", "solo@ex.com"][Math.floor(rnd() * 3)];
      rows.push({
        id: `r${i}`,
        parent_ids: [],
        summary: "",
        author_name: name,
        author_email: email,
        timestamp: 1,
        lane: 0,
        color_index: 0,
        active_lanes: [0],
        active_lane_colors: [0],
        connections: [],
        is_merge: false,
        is_root: false,
      });
      expected.push(authorIdentity(name, email).initials);
    }
    const renderer = new GraphRenderer({ rowHeight: 20 });
    const { ctx, calls } = makeRecordingCtx(500 * 20);
    renderer.render(ctx, rows, 0, rows.length, 0, undefined, {
      theme: THEME,
      viewportHeight: 500 * 20,
      showAvatars: true,
      avatarX: 30,
    });
    const labels = calls.filter((c) => c.op === "fillText") as Array<{ text: string }>;
    expect(labels.map((l) => l.text)).toEqual(expected);
  });
});
