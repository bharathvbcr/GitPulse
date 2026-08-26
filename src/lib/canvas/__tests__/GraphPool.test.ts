import { describe, expect, it } from "vitest";
import { GraphRenderer, type VisualCommitRow } from "../GraphRenderer";
import { makeRecordingCtx, row } from "./recordingCtx";

/**
 * Regression for the lane-run pool aliasing bug (pre-fix behaviour verified
 * by execution during the audit: a render ending with open pass-through runs
 * pushed each into the pool TWICE; the next render then handed one recycled
 * object to two lanes and the second acquireRun overwrote the first lane's
 * geometry — its track silently vanished).
 *
 * The pre-existing guard test compared two renders OF THE SAME INPUT, which
 * are identically corrupted under the bug and therefore always equal. This
 * one poisons the pool, then asserts a DIFFERENT frame against first-principles
 * coordinates.
 */
describe("lane-run pool integrity across renders", () => {
  function trackedRow(lane: number, activeLanes: number[]): VisualCommitRow {
    return row({
      id: `t${lane}-${activeLanes.join("_")}`,
      lane,
      active_lanes: activeLanes,
      active_lane_colors: [...activeLanes],
    });
  }

  function collectVerticalTracks(calls: Array<Record<string, unknown>>): Array<{ x: number; y0: number; y1: number }> {
    const tracks: Array<{ x: number; y0: number; y1: number }> = [];
    let pending: { x: number; y: number } | null = null;
    for (const call of calls) {
      if (call.op === "moveTo") pending = { x: call.x as number, y: call.y as number };
      else if (call.op === "lineTo" && pending && (call.y as number) !== pending.y) {
        tracks.push({ x: pending.x, y0: pending.y, y1: call.y as number });
        pending = null;
      }
    }
    return tracks;
  }

  it("keeps every lane's track at its own x after a pool-poisoning frame", () => {
    const poisonRows: VisualCommitRow[] = [
      trackedRow(0, [0, 1]),
      trackedRow(1, [1, 2]), // ends with lanes 1 AND 2 open → flushed at render end
      trackedRow(2, [2]),
    ];
    const targetRows: VisualCommitRow[] = [
      trackedRow(0, [1, 3]),
      trackedRow(0, [1, 3]),
      trackedRow(0, [1, 3]),
    ];

    const renderer = new GraphRenderer({ rowHeight: 20, laneWidth: 10, originX: 10 });
    renderer.render(makeRecordingCtx().ctx, poisonRows, 0, poisonRows.length, 0);

    const { ctx, calls } = makeRecordingCtx();
    renderer.render(ctx, targetRows, 0, targetRows.length, 0);

    const tracks = collectVerticalTracks(calls);
    // Pass-through lanes 1 and 3 → x=20 and x=40, each spanning all 3 rows.
    // Under the bug, one of these runs recycled the OTHER's object and both
    // stroked identical geometry (one lane's track disappeared or duplicated).
    const laneOne = tracks.filter((t) => t.x === 20);
    const laneThree = tracks.filter((t) => t.x === 40);
    expect(laneOne.length).toBeGreaterThanOrEqual(1);
    expect(laneThree.length).toBeGreaterThanOrEqual(1);

    const laneOneSpan = Math.max(...laneOne.map((t) => t.y1)) - Math.min(...laneOne.map((t) => t.y0));
    const laneThreeSpan = Math.max(...laneThree.map((t) => t.y1)) - Math.min(...laneThree.map((t) => t.y0));
    expect(laneOneSpan).toBeCloseTo(60, 0);   // 3 rows × 20px
    expect(laneThreeSpan).toBeCloseTo(60, 0);
  });

  it("renders distinct frames identically after many alternating renders", () => {
    const a: VisualCommitRow[] = [trackedRow(0, [0, 1]), trackedRow(1, [1])];
    const b: VisualCommitRow[] = [trackedRow(2, [2]), trackedRow(2, [2])];
    const renderer = new GraphRenderer({ rowHeight: 20 });

    // Interleave enough renders to cycle the pool several times.
    for (let i = 0; i < 6; i++) {
      renderer.render(makeRecordingCtx().ctx, a, 0, a.length, 0);
      renderer.render(makeRecordingCtx().ctx, b, 0, b.length, 0);
    }
    const traceB = () => {
      const { ctx, calls } = makeRecordingCtx();
      renderer.render(ctx, b, 0, b.length, 0);
      return JSON.stringify(collectVerticalTracks(calls));
    };
    const reference = (() => {
      const fresh = new GraphRenderer({ rowHeight: 20 });
      const { ctx, calls } = makeRecordingCtx();
      fresh.render(ctx, b, 0, b.length, 0);
      return JSON.stringify(collectVerticalTracks(calls));
    })();
    // After churn, a warm renderer must produce the SAME tracks as a cold one.
    expect(traceB()).toBe(reference);
  });
});
