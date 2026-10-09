import { describe, expect, it } from "vitest";
import {
  cardFace,
  dragExceeded,
  grabPoint,
  insertIndexFromY,
  insertionNeighbors,
  insertionPosition,
  neighborStatus,
  parseColumnStatus,
  shouldCommitMove,
  TASK_DRAG_THRESHOLD_PX,
} from "./boardDrag";

describe("boardDrag", () => {
  it("starts a drag only after the pointer has actually moved", () => {
    expect(dragExceeded(0, 0)).toBe(false);
    expect(dragExceeded(TASK_DRAG_THRESHOLD_PX - 1, 0)).toBe(false);
    expect(dragExceeded(TASK_DRAG_THRESHOLD_PX, 0)).toBe(true);
    expect(dragExceeded(4, 5)).toBe(true);
  });

  it("parses column status from the data attribute and rejects unknown values", () => {
    expect(parseColumnStatus("review")).toBe("review");
    expect(parseColumnStatus("probably_done")).toBeNull();
    expect(parseColumnStatus(null)).toBeNull();
  });

  it("moves a focused card to the neighbouring column", () => {
    expect(neighborStatus("inbox", 1)).toBe("backlog");
    expect(neighborStatus("inbox", -1)).toBeNull();
    expect(neighborStatus("done", 1)).toBeNull();
    expect(neighborStatus("review", -1)).toBe("in_progress");
  });
});

describe("insertionPosition", () => {
  it("places a card strictly between two neighbors instead of appending Date.now()", () => {
    const now = 1_700_000_000_000;
    const position = insertionPosition(1_000, 2_000, now);
    expect(position).toBeGreaterThan(1_000);
    expect(position).toBeLessThan(2_000);
    expect(position).toBe(1_500);
    expect(Number.isSafeInteger(position)).toBe(true);
    expect(position).not.toBe(now);
  });

  it("uses now only for an empty column and appends after the last card", () => {
    const now = 9_000;
    expect(insertionPosition(null, null, now)).toBe(now);
    expect(insertionPosition(400, null, now)).toBe(401);
    expect(insertionPosition(null, 400, now)).toBe(200);
  });

  it("reads neighbors from the remaining cards at an insert index", () => {
    const items = [
      { id: "a", position: 100 },
      { id: "b", position: 200 },
      { id: "c", position: 300 },
    ];
    expect(insertionNeighbors(items, "b", 1)).toEqual({ before: 100, after: 300 });
    expect(insertionNeighbors(items, "b", 0)).toEqual({ before: null, after: 100 });
    expect(insertionNeighbors(items, "b", 2)).toEqual({ before: 300, after: null });
  });

  it("maps a pointer Y onto an insert index from remaining card midpoints", () => {
    expect(insertIndexFromY([10, 30, 50], 5)).toBe(0);
    expect(insertIndexFromY([10, 30, 50], 35)).toBe(2);
    expect(insertIndexFromY([10, 30, 50], 80)).toBe(3);
    expect(insertIndexFromY([], 0)).toBe(0);
  });

  it("commits a same-column drop only when the insert index actually moved", () => {
    expect(shouldCommitMove("ready", "ready", { moving: false, fromIndex: 1, insertIndex: 1 })).toBe(false);
    expect(shouldCommitMove("ready", "ready", { moving: false, fromIndex: 1, insertIndex: 3 })).toBe(true);
    expect(shouldCommitMove("ready", "done", { moving: false, fromIndex: 1, insertIndex: 0 })).toBe(true);
    expect(shouldCommitMove("ready", "done", { moving: true, fromIndex: 1, insertIndex: 0 })).toBe(false);
  });
});

describe("cardFace", () => {
  const names = (id: string) => ({ r1: "GitPulse", r2: "Manvi" }[id]);

  it("leads with the title, pips only P0/P1, and returns at most one repo name", () => {
    const p0 = cardFace(
      { title: "Fix overlay", priority: 0, repository_ids: ["r1", "r2"], labels: ["ui", "drag", "extra"] },
      names,
    );
    expect(p0.title).toBe("Fix overlay");
    expect(p0.pip).toBe(0);
    expect(p0.repo).toBe("GitPulse");
    expect(p0.labels).toEqual(["ui", "drag"]);

    const p1 = cardFace({ title: "T", priority: 1, repository_ids: ["r2"], labels: [] }, names);
    expect(p1.pip).toBe(1);
    expect(p1.repo).toBe("Manvi");
  });

  it("omits Normal and Low text and does not invent a pip for them", () => {
    for (const priority of [2, 3]) {
      const face = cardFace({ title: "Quiet", priority, repository_ids: [], labels: [] }, names);
      expect(face.pip).toBeNull();
      expect(JSON.stringify(face)).not.toMatch(/Normal|Low|Urgent|High/i);
      expect(face.repo).toBeNull();
    }
  });

  it("keeps the point a card was grabbed by under the pointer", () => {
    const box = { left: 300, top: 120, width: 240, height: 64 };
    expect(grabPoint({ x: 492, y: 158 }, box)).toEqual({ dx: 192, dy: 38, width: 240, height: 64 });
    expect(grabPoint({ x: 300, y: 120 }, box)).toEqual({ dx: 0, dy: 0, width: 240, height: 64 });
    expect(grabPoint({ x: 540, y: 184 }, box)).toEqual({ dx: 240, dy: 64, width: 240, height: 64 });
  });

  it("clamps a press outside the measured box onto its edge", () => {
    const box = { left: 300, top: 120, width: 240, height: 64 };
    expect(grabPoint({ x: 10, y: 10 }, box)).toEqual({ dx: 0, dy: 0, width: 240, height: 64 });
    expect(grabPoint({ x: 9_000, y: 9_000 }, box)).toEqual({ dx: 240, dy: 64, width: 240, height: 64 });
  });

  it("never produces a non-finite or negative preview from a degenerate measurement", () => {
    const bad = [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY, -5, 0];
    for (const width of bad) for (const left of bad) for (const x of bad) {
      const grab = grabPoint({ x, y: x }, { left, top: left, width, height: width });
      for (const value of Object.values(grab)) {
        expect(Number.isFinite(value)).toBe(true);
        expect(value).toBeGreaterThanOrEqual(0);
      }
      expect(grab.dx).toBeLessThanOrEqual(grab.width);
      expect(grab.dy).toBeLessThanOrEqual(grab.height);
    }
  });

  it("puts the pointer inside the preview for any press and any pointer position", () => {
    let seed = 7;
    const rand = () => ((seed = (seed * 1_103_515_245 + 12_345) % 2 ** 31) / 2 ** 31);
    for (let i = 0; i < 5_000; i++) {
      const box = { left: rand() * 2_000 - 500, top: rand() * 2_000 - 500, width: rand() * 600, height: rand() * 300 };
      const grab = grabPoint({ x: box.left + rand() * box.width, y: box.top + rand() * box.height }, box);
      const pointer = { x: rand() * 4_000 - 1_000, y: rand() * 4_000 - 1_000 };
      const left = pointer.x - grab.dx, top = pointer.y - grab.dy;
      expect(pointer.x).toBeGreaterThanOrEqual(left - 1e-9);
      expect(pointer.x).toBeLessThanOrEqual(left + grab.width + 1e-9);
      expect(pointer.y).toBeGreaterThanOrEqual(top - 1e-9);
      expect(pointer.y).toBeLessThanOrEqual(top + grab.height + 1e-9);
    }
  });
});
