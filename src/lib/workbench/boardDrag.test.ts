import { describe, expect, it } from "vitest";
import {
  cardFace,
  dragExceeded,
  dropTargetStatus,
  insertIndexFromY,
  insertionNeighbors,
  insertionPosition,
  neighborStatus,
  parseColumnStatus,
  shouldCommitMove,
  TASK_DRAG_THRESHOLD_PX,
  visibleStatuses,
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

  it("accepts a drop only onto a different column while idle", () => {
    expect(dropTargetStatus("ready", "ready", { moving: false })).toBeNull();
    expect(dropTargetStatus("ready", null, { moving: false })).toBeNull();
    expect(dropTargetStatus("ready", "done", { moving: true })).toBeNull();
    expect(dropTargetStatus("ready", "done", { moving: false })).toBe("done");
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

describe("visibleStatuses", () => {
  it("hides empty statuses while idle and still exposes them as drop targets while dragging", () => {
    const counts = { ready: 2, done: 1 };
    expect(visibleStatuses(counts, false)).toEqual(["ready", "done"]);
    expect(visibleStatuses(counts, true)).toEqual([
      "inbox",
      "backlog",
      "ready",
      "in_progress",
      "review",
      "done",
    ]);
    expect(visibleStatuses(counts, true)).toContain("inbox");
  });

  it("keeps every column when the board is empty so drop targets still exist", () => {
    expect(visibleStatuses({}, false)).toHaveLength(6);
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
});
