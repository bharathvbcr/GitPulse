import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  MAX_TASK_TABS,
  activateTaskTab,
  canOpenTaskTab,
  closeTaskTab,
  emptyTaskChrome,
  emptyTaskTabs,
  openSavedTaskIds,
  openTaskTab,
  publishTaskChrome,
  retargetTaskTab,
  taskChrome,
  taskTabLabel,
  updateTaskTab,
  type TaskTab,
  type TaskTabState,
} from "./taskTabs";

const tab = (id: string, overrides: Partial<TaskTab> = {}): TaskTab => ({
  id,
  title: id,
  status: "ready",
  draft: false,
  ...overrides,
});

function fill(count: number): TaskTabState {
  let state = emptyTaskTabs();
  for (let i = 0; i < count; i += 1) state = openTaskTab(state, tab(`t${i}`));
  return state;
}

describe("task tab model", () => {
  it("opens into an empty strip and focuses the new tab", () => {
    const state = openTaskTab(emptyTaskTabs(), tab("a", { title: "Ship the board" }));
    expect(state.tabs).toHaveLength(1);
    expect(state.activeId).toBe("a");
    expect(state.tabs[0]?.title).toBe("Ship the board");
  });

  it("reopening an existing id focuses it and refreshes its title", () => {
    let state = openTaskTab(emptyTaskTabs(), tab("a", { title: "Old" }));
    state = openTaskTab(state, tab("b"));
    const next = openTaskTab(state, tab("a", { title: "New", status: "review" }));
    expect(next.tabs).toHaveLength(2);
    expect(next.activeId).toBe("a");
    expect(next.tabs[0]).toMatchObject({ title: "New", status: "review" });
  });

  it("refuses a new tab at the ceiling but still focuses one already open", () => {
    const full = fill(MAX_TASK_TABS);
    expect(full.tabs).toHaveLength(MAX_TASK_TABS);
    expect(canOpenTaskTab(full, "fresh")).toBe(false);
    expect(openTaskTab(full, tab("fresh"))).toBe(full);
    const first = full.tabs[0]!.id;
    const repeated = openTaskTab(full, tab(first, { title: "Still here" }));
    expect(repeated.tabs).toHaveLength(MAX_TASK_TABS);
    expect(repeated.activeId).toBe(first);
    expect(repeated.tabs[0]?.title).toBe("Still here");
  });

  it("ignores an empty id rather than minting a nameless tab", () => {
    const state = emptyTaskTabs();
    expect(openTaskTab(state, tab(""))).toBe(state);
    expect(canOpenTaskTab(state, "")).toBe(false);
  });

  it("closes the active tab toward the right, then the left, then empty", () => {
    let state = fill(3);
    expect(state.activeId).toBe("t2");
    state = closeTaskTab(state, "t2");
    expect(state.tabs.map((item) => item.id)).toEqual(["t0", "t1"]);
    expect(state.activeId).toBe("t1");
    state = closeTaskTab(state, "t1");
    expect(state.activeId).toBe("t0");
    state = closeTaskTab(state, "t0");
    expect(state).toEqual(emptyTaskTabs());
  });

  it("closing an inactive tab does not steal focus", () => {
    let state = fill(3);
    const active = state.activeId;
    state = closeTaskTab(state, "t0");
    expect(state.activeId).toBe(active);
    expect(state.tabs.map((item) => item.id)).toEqual(["t1", "t2"]);
  });

  it("closing a missing id is a no-op", () => {
    const state = fill(2);
    expect(closeTaskTab(state, "missing")).toBe(state);
  });

  it("activate refuses unknown ids and is a no-op when already focused", () => {
    const state = fill(2);
    expect(activateTaskTab(state, "missing")).toBe(state);
    expect(activateTaskTab(state, state.activeId!)).toBe(state);
    expect(activateTaskTab(state, "t0").activeId).toBe("t0");
  });

  it("retargets a saved draft onto the new id, or focuses a collision instead of duplicating", () => {
    let state = openTaskTab(emptyTaskTabs(), tab("draft-1", { draft: true, title: "New task" }));
    state = openTaskTab(state, tab("saved"));
    const renamed = retargetTaskTab(state, "draft-1", tab("created", { title: "Created", status: "inbox" }));
    expect(renamed.tabs.map((item) => item.id)).toEqual(["created", "saved"]);
    expect(renamed.tabs[0]).toMatchObject({ title: "Created", draft: false, status: "inbox" });

    const clash = retargetTaskTab(renamed, "created", tab("saved", { title: "Saved" }));
    expect(clash.tabs.map((item) => item.id)).toEqual(["saved"]);
    expect(clash.activeId).toBe("saved");
  });

  it("update and label stay bounded; drafts without a title read as New task", () => {
    let state = openTaskTab(emptyTaskTabs(), tab("a", { title: "Short" }));
    state = updateTaskTab(state, "a", { title: "x".repeat(80), status: "in_progress" });
    expect(taskTabLabel(state.tabs[0]!)).toHaveLength(28);
    expect(taskTabLabel(tab("d", { draft: true, title: "   " }))).toBe("New task");
    expect(updateTaskTab(state, "missing", { title: "nope" })).toBe(state);
  });

  it("only saved tabs count as open cards on the board", () => {
    let state = openTaskTab(emptyTaskTabs(), tab("draft-1", { draft: true }));
    state = openTaskTab(state, tab("real"));
    expect([...openSavedTaskIds(state)]).toEqual(["real"]);
  });

  it("publishes a clamped chrome snapshot and drops non-finite counts", () => {
    publishTaskChrome({ openTabs: 3.9, inProgress: 2 });
    expect(get(taskChrome)).toEqual({ openTabs: 3, inProgress: 2 });
    publishTaskChrome({ openTabs: -4, inProgress: Number.NaN });
    expect(get(taskChrome)).toEqual(emptyTaskChrome());
    publishTaskChrome({ openTabs: Number.POSITIVE_INFINITY, inProgress: 1e9 });
    expect(get(taskChrome).inProgress).toBe(10_000);
    expect(get(taskChrome).openTabs).toBe(0);
  });

  it("survives a dense open/close/retarget sequence without duplicating ids", () => {
    let state = emptyTaskTabs();
    for (let i = 0; i < 200; i += 1) {
      const id = `t${i % (MAX_TASK_TABS + 3)}`;
      state = openTaskTab(state, tab(id, { draft: i % 7 === 0, title: `Task ${i}` }));
      if (i % 5 === 0) state = closeTaskTab(state, id);
      if (i % 11 === 0 && state.tabs[0]) {
        state = retargetTaskTab(state, state.tabs[0].id, tab(`saved-${i}`, { title: "Saved" }));
      }
      const ids = state.tabs.map((item) => item.id);
      expect(new Set(ids).size).toBe(ids.length);
      if (state.activeId) expect(ids).toContain(state.activeId);
      expect(state.tabs.length).toBeLessThanOrEqual(MAX_TASK_TABS);
    }
  });
});
