import { get } from "svelte/store";
import { afterEach, describe, expect, it, vi } from "vitest";
import { openSuspectsFor } from "../codeintel/openSuspects";
import { interfaceStore } from "../stores/interfaceStore";
import { repoStore, type RepoState } from "../stores/repoStore";
import { openConflicts } from "../ui/workConflicts";
import { buildCommands } from "./catalog";

vi.mock("../codeintel/openSuspects", () => ({ openSuspectsFor: vi.fn() }));
vi.mock("../ui/workConflicts", () => ({ openConflicts: vi.fn() }));

const snapshot = (patch: Partial<RepoState> = {}): RepoState => ({ ...get(repoStore), ...patch });
const ready = (patch: Partial<RepoState> = {}) =>
  snapshot({
    activeTabId: "tab-1", currentPath: "/repo", currentBranch: "main", isLoading: false, isBare: false,
    operation: { operation: "merge", probeFailed: false }, ...patch,
  } as Partial<RepoState>);

afterEach(() => vi.restoreAllMocks());

const find = (state: RepoState, id: string) => {
  const command = buildCommands(state, () => {}).find((item) => item.id === id);
  expect(command, id).toBeDefined();
  return command!;
};

describe("palette commands that hand off to the stores and views", () => {
  it("opens the suspects view for the whole symptom, and for the open file when one is selected", async () => {
    await find(ready(), "history:suspects").action();
    expect(openSuspectsFor).toHaveBeenLastCalledWith();

    const withFile = ready({ selectedFilePath: "src/a.ts" });
    const command = find(withFile, "history:suspects-file");
    expect(command.disabledReason).toBeUndefined();
    expect(command.description).toContain("src/a.ts");
    await command.action();
    expect(openSuspectsFor).toHaveBeenLastCalledWith("src/a.ts");
  });

  it("refuses the file suspects command until a file is open", () => {
    const command = find(ready({ selectedFilePath: null }), "history:suspects-file");
    expect(command.disabledReason).toBe("Open a file in Explorer first");
    expect(command.description).toContain("Open a file in Explorer first");
  });

  it("opens the conflict editor on the repository surface while an operation is parked", async () => {
    const surface = vi.spyOn(interfaceStore, "setGlobalSurface").mockImplementation(() => {});
    const command = find(ready(), "work:conflicts");
    expect(command.disabledReason).toBeUndefined();
    await command.action();
    expect(surface).toHaveBeenCalledWith("repository");
    expect(openConflicts).toHaveBeenCalledTimes(1);
  });

  it("opens the agents plane", async () => {
    const open = vi.spyOn(interfaceStore, "setAgentsOpen").mockImplementation(() => {});
    await find(ready(), "agents").action();
    expect(open).toHaveBeenCalledWith(true);
  });

  it("pulls with rebase against the repository the palette was built for", async () => {
    const pull = vi.spyOn(repoStore, "pull").mockResolvedValue(undefined as never);
    await find(ready(), "pull-rebase").action();
    expect(pull).toHaveBeenCalledWith(undefined, undefined, true, { tabId: "tab-1", path: "/repo" });
  });

  it("drives each repository-group command through the store", async () => {
    const calls = {
      group: vi.spyOn(repoStore, "groupByParentFolder").mockImplementation(() => {}),
      collapseAll: vi.spyOn(repoStore, "collapseAllGroups").mockImplementation(() => {}),
      collapseInactive: vi.spyOn(repoStore, "collapseInactiveGroups").mockImplementation(() => {}),
      expand: vi.spyOn(repoStore, "expandAllGroups").mockImplementation(() => {}),
      ungroup: vi.spyOn(repoStore, "ungroupTabs").mockImplementation(() => {}),
    };
    const state = ready();
    await find(state, "group_by_parent_folder").action();
    await find(state, "collapse_all_groups").action();
    await find(state, "collapse_inactive_groups").action();
    await find(state, "expand_all_groups").action();
    await find(state, "ungroup_all").action();
    for (const [name, spy] of Object.entries(calls)) expect(spy, name).toHaveBeenCalledTimes(1);
  });

  it("flips collapsing of inactive group tabs from its current setting, and toggles the shell rule", async () => {
    const setCollapse = vi.spyOn(interfaceStore, "setCollapseInactiveGroupTabs").mockImplementation(() => {});
    const toggleShells = vi.spyOn(interfaceStore, "toggleCollapseInactiveIncludeShells").mockImplementation(() => {});
    const current = get(interfaceStore).collapseInactiveGroupTabs;
    await find(ready(), "collapse_inactive_group_tabs").action();
    expect(setCollapse).toHaveBeenCalledWith(!current);
    await find(ready(), "collapse_inactive_include_shells").action();
    expect(toggleShells).toHaveBeenCalledTimes(1);
  });
});
