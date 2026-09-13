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

  it("gives a saved task the four panes, in a fixed order", () => {
    expect(editorTabs(true).map((tab) => tab.id)).toEqual(["task", "organize", "agent", "ai"]);
    expect(editorTabs(true).map((tab) => tab.label)).toEqual(["Task", "Organize", "Agent", "AI"]);
  });

  it("hands back a fresh array each call so a caller cannot reorder the source", () => {
    const first = editorTabs(true);
    first.reverse();
    expect(editorTabs(true).map((tab) => tab.id)).toEqual(["task", "organize", "agent", "ai"]);
  });

  it("describes every pane it offers", () => {
    for (const tab of editorTabs(true)) {
      expect(tab.hint.length, tab.id).toBeGreaterThan(10);
      expect(editorTabHint(tab.id)).toBe(tab.hint);
    }
    expect(editorTabHint("nonsense" as never)).toBe("");
  });
});

describe("resolveEditorTab", () => {
  it("keeps the reader where they were when a draft becomes a saved task", () => {
    expect(resolveEditorTab("task", true)).toBe("task");
    expect(resolveEditorTab("organize", true)).toBe("organize");
  });

  it("falls back to Task for a pane this sheet does not offer", () => {
    expect(resolveEditorTab("agent", false)).toBe("task");
    expect(resolveEditorTab("ai", false)).toBe("task");
    expect(resolveEditorTab("organize", false)).toBe("task");
  });

  it("falls back to Task for anything it does not recognize", () => {
    expect(resolveEditorTab(undefined, true)).toBe("task");
    expect(resolveEditorTab(null, true)).toBe("task");
    expect(resolveEditorTab(7, true)).toBe("task");
    expect(resolveEditorTab("runs", true)).toBe("task");
    expect(isTaskEditorTab("ai")).toBe(true);
    expect(isTaskEditorTab("runs")).toBe(false);
  });
});

describe("editorTabBadge", () => {
  it("counts only things that already exist", () => {
    expect(editorTabBadge("agent", { runs: 3 })).toBe(3);
    expect(editorTabBadge("ai", { suggestions: 2 })).toBe(2);
    expect(editorTabBadge("task", { runs: 3, suggestions: 2 })).toBe(0);
    expect(editorTabBadge("organize", { runs: 3, suggestions: 2 })).toBe(0);
  });

  it("shows nothing for zero, missing, negative or nonsense counts", () => {
    expect(editorTabBadge("agent", {})).toBe(0);
    expect(editorTabBadge("agent", { runs: 0 })).toBe(0);
    expect(editorTabBadge("agent", { runs: -5 })).toBe(0);
    expect(editorTabBadge("agent", { runs: Number.NaN })).toBe(0);
    expect(editorTabBadge("ai", { suggestions: Number.POSITIVE_INFINITY })).toBe(0);
  });

  it("caps a runaway count instead of widening the tab strip", () => {
    expect(editorTabBadge("agent", { runs: 5_000 })).toBe(99);
    expect(editorTabBadge("ai", { suggestions: 12.9 })).toBe(12);
  });
});
