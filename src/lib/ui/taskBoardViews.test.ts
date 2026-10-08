import { describe, expect, it } from "vitest";
import {
  MAX_BOARDS,
  MAX_VIEWS_PER_BOARD,
  boardKey,
  boardPrefs,
  deleteView,
  effectiveSwimlane,
  sameSnapshot,
  sanitizeBoards,
  sanitizeSnapshot,
  sanitizeWipLimit,
  sanitizeWipLimits,
  saveView,
  setWipLimit,
  viewName,
  wipState,
  type TaskViewSnapshot,
} from "./taskBoardViews";
import { emptyFacet } from "../workbench/taskOrganize";

const snapshot = (overrides: Partial<TaskViewSnapshot> = {}): TaskViewSnapshot => ({
  layout: "board", density: "comfortable", swimlane: "none", hiddenColumns: [], cardFields: ["repo", "type", "owner", "due", "labels"], facet: emptyFacet(), search: "", ...overrides,
});
let ids = 0;
const nextId = () => `view-${++ids}`;

describe("work-in-progress limits", () => {
  it("accepts whole limits from 1 to 999 and nothing else", () => {
    expect([1, 999, "12"].map(sanitizeWipLimit)).toEqual([1, 999, 12]);
    for (const bad of [0, -1, 1000, 2.5, "", " ", "x", NaN, Infinity, null, undefined, {}]) expect(sanitizeWipLimit(bad)).toBeNull();
  });
  it("judges the column total, not the page: at the limit is not over it", () => {
    expect(wipState(30, 35)).toBe("under");
    expect(wipState(35, 35)).toBe("at");
    expect(wipState(40, 35)).toBe("over");
    expect(wipState(40, null)).toBeNull();
    expect(wipState(undefined, 3)).toBe("under");
  });
  it("drops unknown statuses and bad limits read from storage", () => {
    expect(sanitizeWipLimits({ ready: 3, bogus: 4, review: 0, in_progress: "2" })).toEqual({ ready: 3, in_progress: 2 });
    expect(sanitizeWipLimits([3])).toEqual({});
  });
  it("sets and clears one board's limit without touching another board", () => {
    let boards = setWipLimit({}, "workspace:a", "ready", 3);
    boards = setWipLimit(boards, "workspace:b", "ready", 5);
    expect(boardPrefs(boards, "workspace:a").wip).toEqual({ ready: 3 });
    boards = setWipLimit(boards, "workspace:a", "ready", null);
    expect(boards["workspace:a"]).toBeUndefined();
    expect(boardPrefs(boards, "workspace:b").wip).toEqual({ ready: 5 });
  });
});

describe("saved views", () => {
  it("keys boards by scope", () => {
    expect(boardKey({ kind: "global" })).toBe("global");
    expect(boardKey({ kind: "workspace", id: "w1" })).toBe("workspace:w1");
    expect(boardKey({ kind: "repository", id: "r1" })).toBe("repository:r1");
  });
  it("saves per board, and the same name replaces that view keeping its id", () => {
    const first = saveView({}, "workspace:a", "  Triage  ", snapshot({ layout: "list" }), nextId);
    expect(first.ok).toBe(true);
    if (!first.ok) return;
    expect(first.view.name).toBe("Triage");
    const again = saveView(first.boards, "workspace:a", "triage", snapshot({ swimlane: "owner" }), nextId);
    expect(again.ok && again.replaced && again.view.id === first.view.id && again.view.swimlane === "owner").toBe(true);
    if (!again.ok) return;
    expect(boardPrefs(again.boards, "workspace:a").views).toHaveLength(1);
    expect(boardPrefs(again.boards, "workspace:b").views).toHaveLength(0);
  });
  it("refuses a blank or overlong name and a twenty-first view, out loud", () => {
    expect(saveView({}, "global", "   ", snapshot(), nextId)).toMatchObject({ ok: false });
    expect(saveView({}, "global", "x".repeat(61), snapshot(), nextId)).toMatchObject({ ok: false });
    let boards = {};
    for (let i = 0; i < MAX_VIEWS_PER_BOARD; i++) {
      const saved = saveView(boards, "global", `View ${i}`, snapshot(), nextId);
      if (saved.ok) boards = saved.boards;
    }
    const over = saveView(boards, "global", "One more", snapshot(), nextId);
    expect(over.ok).toBe(false);
    if (!over.ok) expect(over.reason).toContain(String(MAX_VIEWS_PER_BOARD));
    expect(saveView(boards, "global", "View 3", snapshot(), nextId).ok).toBe(true);
  });
  it("refuses a board key it does not recognise", () => {
    expect(saveView({}, "../../etc", "Name", snapshot(), nextId).ok).toBe(false);
  });
  it("deletes one view and drops an emptied board", () => {
    const saved = saveView({}, "global", "Mine", snapshot(), nextId);
    if (!saved.ok) throw new Error("not saved");
    expect(deleteView(saved.boards, "global", saved.view.id)).toEqual({});
  });
  it("reads hostile storage into views the board can draw", () => {
    const boards = sanitizeBoards({
      global: { views: [
        { id: "a", name: "Kept", layout: "list", swimlane: "label", hiddenColumns: ["inbox", "nope"], facet: { priority: "1", due: "never" }, search: 7 },
        { id: "a", name: "Same id" },
        { id: "b", name: "kept" },
        { id: "bad id!", name: "Bad" },
        { id: "c", name: "" },
      ], wip: { ready: 2 } },
      "nonsense": { views: [{ id: "z", name: "Z" }] },
      "workspace:empty": { views: [], wip: {} },
    });
    expect(Object.keys(boards)).toEqual(["global"]);
    const [view] = boards.global.views;
    expect(boards.global.views).toHaveLength(1);
    expect(view).toMatchObject({ id: "a", name: "Kept", layout: "list", swimlane: "label", hiddenColumns: ["inbox"], search: "" });
    expect(view.facet).toEqual({ ...emptyFacet(), priority: 1 });
    expect(boards.global.wip).toEqual({ ready: 2 });
  });
  it("bounds the number of boards kept", () => {
    const many = Object.fromEntries(Array.from({ length: MAX_BOARDS + 20 }, (_, i) => [`workspace:w${i}`, { wip: { ready: 1 } }]));
    expect(Object.keys(sanitizeBoards(many))).toHaveLength(MAX_BOARDS);
  });
  it("compares what a board shows, not how it was stored", () => {
    expect(sameSnapshot(snapshot({ hiddenColumns: ["done", "inbox"] }), snapshot({ hiddenColumns: ["inbox", "done"] }))).toBe(true);
    expect(sameSnapshot(snapshot({ search: "a" }), snapshot({ search: "b" }))).toBe(false);
    expect(sanitizeSnapshot(null)).toEqual(snapshot());
  });
  it("collapses whitespace in names", () => {
    expect(viewName(" My \n view ")).toBe("My view");
    expect(viewName(3)).toBe("");
  });
});

describe("swimlanes", () => {
  it("draws no status lanes on the board, whose columns are statuses already", () => {
    expect(effectiveSwimlane("board", "status")).toBe("none");
    expect(effectiveSwimlane("list", "status")).toBe("status");
    expect(effectiveSwimlane("board", "owner")).toBe("owner");
  });
});
