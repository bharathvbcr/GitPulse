import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { interfaceStore } from "../interfaceStore";
import type { TaskViewSnapshot } from "../../ui/taskBoardViews";

const snapshot = (over: Partial<TaskViewSnapshot> = {}): TaskViewSnapshot => ({
  layout: "list",
  density: "compact",
  swimlane: "none",
  order: "priority",
  hiddenColumns: ["done"],
  cardFields: ["repo", "owner"],
  facet: { priority: "all", kind: "all", owner: "all", label: "all", severity: "all", due: "all", repository: "all" },
  search: "",
  ...over,
});

beforeEach(() => interfaceStore.reset());

describe("agents plane preferences", () => {
  it("hides a column on the first toggle, shows it on the second, and shows every column in one action", () => {
    interfaceStore.toggleAgentsColumn("changes");
    interfaceStore.toggleAgentsColumn("branch");
    expect(get(interfaceStore).agentsHiddenColumns).toEqual(["changes", "branch"]);
    interfaceStore.toggleAgentsColumn("changes");
    expect(get(interfaceStore).agentsHiddenColumns).toEqual(["branch"]);
    interfaceStore.showAllAgentsColumns();
    expect(get(interfaceStore).agentsHiddenColumns).toEqual([]);
  });

  it("flips compact rows and remembers the filter", () => {
    const before = get(interfaceStore).agentsCompact;
    interfaceStore.toggleAgentsCompact();
    expect(get(interfaceStore).agentsCompact).toBe(!before);
    interfaceStore.setAgentsFilter("attention");
    expect(get(interfaceStore).agentsFilter).toBe("attention");
  });
});

describe("task board preferences", () => {
  it("hides one repository's tasks and brings every repository back", () => {
    interfaceStore.toggleTaskRepositoryHidden("repo-a");
    interfaceStore.toggleTaskRepositoryHidden("repo-b");
    expect(get(interfaceStore).taskHiddenRepositories).toEqual(["repo-a", "repo-b"]);
    interfaceStore.toggleTaskRepositoryHidden("repo-a");
    expect(get(interfaceStore).taskHiddenRepositories).toEqual(["repo-b"]);
    interfaceStore.showAllTaskRepositories();
    expect(get(interfaceStore).taskHiddenRepositories).toEqual([]);
  });

  it("sanitizes card fields on the way in, whether set whole or toggled", () => {
    interfaceStore.setTaskCardFields(["repo", "due"]);
    expect(get(interfaceStore).taskCardFields).toEqual(["repo", "due"]);
    interfaceStore.toggleTaskCardField("labels");
    expect(get(interfaceStore).taskCardFields).toContain("labels");
    interfaceStore.toggleTaskCardField("labels");
    expect(get(interfaceStore).taskCardFields).not.toContain("labels");
  });

  it("accepts a known swimlane or order and falls back to the default for anything else", () => {
    interfaceStore.setTaskSwimlane("owner" as never);
    const lane = get(interfaceStore).taskSwimlane;
    interfaceStore.setTaskSwimlane("nonsense" as never);
    expect(get(interfaceStore).taskSwimlane).toBe("none");
    interfaceStore.setTaskSwimlane(lane);

    const defaultOrder = (interfaceStore.reset(), get(interfaceStore).taskOrder);
    interfaceStore.setTaskOrder("nonsense" as never);
    expect(get(interfaceStore).taskOrder).toBe(defaultOrder);
  });

  it("keeps a work-in-progress limit per board and removes it with null", () => {
    interfaceStore.setTaskWipLimit("global", "in_progress", 3);
    expect(get(interfaceStore).taskBoards.global.wip.in_progress).toBe(3);
    interfaceStore.setTaskWipLimit("global", "in_progress", null);
    expect(get(interfaceStore).taskBoards.global?.wip.in_progress).toBeUndefined();
  });

  it("saves a view under a name, replaces it by that name, applies it, and deletes it", () => {
    let n = 0;
    const newId = () => `view-${(n += 1)}`;
    const saved = interfaceStore.saveTaskView("global", "Mine", snapshot(), newId);
    expect(saved.ok).toBe(true);
    expect(get(interfaceStore).taskBoards.global.views.map((v) => v.name)).toEqual(["Mine"]);

    const replaced = interfaceStore.saveTaskView("global", "mine", snapshot({ density: "comfortable" }), newId);
    expect(replaced.ok && replaced.replaced).toBe(true);
    expect(get(interfaceStore).taskBoards.global.views).toHaveLength(1);

    const refused = interfaceStore.saveTaskView("not a board", "X", snapshot(), newId);
    expect(refused.ok).toBe(false);
    expect(get(interfaceStore).taskBoards.global.views).toHaveLength(1);

    const view = get(interfaceStore).taskBoards.global.views[0];
    interfaceStore.applyTaskView({ ...view, layout: "list", density: "compact", hiddenColumns: ["done"], cardFields: ["repo"] });
    expect(get(interfaceStore)).toMatchObject({ taskLayout: "list", taskDensity: "compact", taskHiddenColumns: ["done"], taskCardFields: ["repo"] });

    interfaceStore.deleteTaskView("global", view.id);
    expect(get(interfaceStore).taskBoards.global?.views ?? []).toEqual([]);
  });
});

describe("appearance and graph preferences", () => {
  it("holds the wallpaper intensity inside its range and treats a non-number as full strength", () => {
    interfaceStore.setWallpaperIntensity(5);
    expect(get(interfaceStore).wallpaperIntensity).toBe(1);
    interfaceStore.setWallpaperIntensity(-3);
    expect(get(interfaceStore).wallpaperIntensity).toBe(0.1);
    interfaceStore.setWallpaperIntensity(Number.NaN);
    expect(get(interfaceStore).wallpaperIntensity).toBe(1);
    interfaceStore.setWallpaperIntensity(0.4);
    expect(get(interfaceStore).wallpaperIntensity).toBe(0.4);
  });

  it("remembers the graph's ref scope and the terminal screen-reader mode", () => {
    interfaceStore.setGraphRefScope("all");
    interfaceStore.setTerminalScreenReader(true);
    expect(get(interfaceStore)).toMatchObject({ graphRefScope: "all", terminalScreenReader: true });
  });
});
