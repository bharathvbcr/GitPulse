import { describe, expect, it } from "vitest";
import {
  editorTabBadge,
  editorTabHint,
  editorTabs,
  isTaskEditorTab,
  resolveEditorTab,
} from "./taskEditorTabs";

describe("editorTabs", () => {
  it("gives a draft exactly one pane, so a new task is never a navigation", () => {
    const tabs = editorTabs(false);
    expect(tabs.map((tab) => tab.id)).toEqual(["task"]);
  });

  it("gives a saved task the two panes, in a fixed order", () => {
    expect(editorTabs(true).map((tab) => tab.id)).toEqual(["task", "agent"]);
    expect(editorTabs(true).map((tab) => tab.label)).toEqual(["Task", "Agent"]);
  });

  it("hands back a fresh array each call so a caller cannot reorder the source", () => {
    const first = editorTabs(true);
    first.reverse();
    expect(editorTabs(true).map((tab) => tab.id)).toEqual(["task", "agent"]);
  });

  it("describes every pane it offers", () => {
    for (const tab of editorTabs(true)) {
      expect(tab.hint.length, tab.id).toBeGreaterThan(10);
      expect(editorTabHint(tab.id)).toBe(tab.hint);
    }
    expect(editorTabHint("nonsense" as never)).toBe("");
  });

  it("does not speak compose copy on a saved task", () => {
    const savedTabs = editorTabs(true);
    const taskSaved = savedTabs.find((t) => t.id === "task")!;
    expect(taskSaved.hint).toBe("What the work is and how it is scheduled.");
    expect(taskSaved.hint).not.toContain("model");
    expect(editorTabHint("task", true)).toBe("What the work is and how it is scheduled.");

    const draftTabs = editorTabs(false);
    const taskDraft = draftTabs.find((t) => t.id === "task")!;
    expect(taskDraft.hint).toContain("model's help with it");
    expect(editorTabHint("task", false)).toContain("model's help with it");
  });
});

describe("resolveEditorTab", () => {
  it("keeps the reader where they were when a draft becomes a saved task", () => {
    expect(resolveEditorTab("task", true)).toBe("task");
    expect(resolveEditorTab("agent", true)).toBe("agent");
  });

  // Organize and AI were folded into Task. A sheet that still asks for them —
  // a session restored from an older build, or the pane a reader was on when
  // the merge shipped — must land on the pane that now holds those fields
  // *because they moved there*, not because the value was unreadable. The two
  // are separate assertions on purpose: the merged ids are named here, and the
  // unrecognized fallback is proven separately below, so a future pane cannot
  // inherit either meaning by accident.
  it("sends a pane it merged away to the pane that absorbed it", () => {
    expect(resolveEditorTab("organize", true)).toBe("task");
    expect(resolveEditorTab("ai", true)).toBe("task");
    expect(isTaskEditorTab("organize")).toBe(false);
    expect(isTaskEditorTab("ai")).toBe(false);
  });

  it("falls back to Task for a pane this sheet does not offer", () => {
    expect(resolveEditorTab("agent", false)).toBe("task");
  });

  it("falls back to Task for anything it does not recognize", () => {
    expect(resolveEditorTab(undefined, true)).toBe("task");
    expect(resolveEditorTab(null, true)).toBe("task");
    expect(resolveEditorTab(7, true)).toBe("task");
    expect(resolveEditorTab("runs", true)).toBe("task");
    expect(isTaskEditorTab("agent")).toBe(true);
    expect(isTaskEditorTab("runs")).toBe(false);
  });

  // The merge table is a plain object lookup, so a stored key that names an
  // Object.prototype member must not resolve through the prototype chain and
  // hand back a function where a pane id belongs.
  it("does not resolve inherited object members as panes", () => {
    for (const key of ["constructor", "toString", "__proto__", "hasOwnProperty"]) {
      expect(resolveEditorTab(key, true), key).toBe("task");
    }
  });
});

describe("editorTabBadge", () => {
  it("counts only things that already exist", () => {
    expect(editorTabBadge("agent", { runs: 3 })).toBe(3);
    expect(editorTabBadge("task", { runs: 3 })).toBe(0);
  });

  it("shows nothing for zero, missing, negative or nonsense counts", () => {
    expect(editorTabBadge("agent", {})).toBe(0);
    expect(editorTabBadge("agent", { runs: 0 })).toBe(0);
    expect(editorTabBadge("agent", { runs: -5 })).toBe(0);
    expect(editorTabBadge("agent", { runs: Number.NaN })).toBe(0);
    expect(editorTabBadge("agent", { runs: Number.POSITIVE_INFINITY })).toBe(0);
  });

  it("caps a runaway count instead of widening the tab strip", () => {
    expect(editorTabBadge("agent", { runs: 5_000 })).toBe(99);
    expect(editorTabBadge("agent", { runs: 12.9 })).toBe(12);
  });
});
