import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { interfaceStore } from "../stores/interfaceStore";
import { repoStore, type RepoState } from "../stores/repoStore";
import { clearActiveSelection, setActiveSelection } from "../terminal/activeSelection";
import { addToTray, contextTrays } from "../terminal/contextTray";
import { terminalLaunchRequests } from "../terminal/launchRequests";
import { explorerLens, resetExplorerLens } from "../files/explorerLens";
import { buildCommands } from "./catalog";

const sendToAgentFromPage = vi.hoisted(() => vi.fn(async () => true));
vi.mock("../terminal/sendFromPage", () => ({ sendToAgentFromPage }));

const snapshot = (patch: Partial<RepoState> = {}): RepoState => ({ ...get(repoStore), ...patch });
const ready = (patch: Partial<RepoState> = {}) =>
  snapshot({
    activeTabId: "tab-1", currentPath: "/repo", currentBranch: "main", isLoading: false, isBare: false,
    operation: { operation: null, probeFailed: false }, ...patch,
  } as Partial<RepoState>);
const find = (state: RepoState, id: string) => {
  const command = buildCommands(state, () => {}).find((item) => item.id === id);
  expect(command, id).toBeDefined();
  return command!;
};
const owner = {};

beforeEach(() => {
  contextTrays.set({});
  sendToAgentFromPage.mockClear();
  vi.spyOn(repoStore, "targetIsActive").mockReturnValue(true);
});
afterEach(() => {
  clearActiveSelection(owner);
  resetExplorerLens();
  vi.restoreAllMocks();
});

describe("palette: send code to an agent", () => {
  it("lists both selection sends with the reason when no lines are selected, and runs them when there are", () => {
    for (const id of ["agent-send-selection", "agent-send-selection-content"]) {
      expect(find(ready(), id).disabledReason).toContain("Select lines");
    }
    const reference = vi.fn();
    const content = vi.fn();
    setActiveSelection(owner, { path: "src/a.ts", startLine: 4, endLine: 9, sendReference: reference, sendContent: content, addToTray() {} });
    const send = find(ready(), "agent-send-selection");
    expect(send.disabledReason).toBeUndefined();
    expect(send.description).toContain("src/a.ts L4–9");
    void send.action();
    expect(reference).toHaveBeenCalledTimes(1);
    void find(ready(), "agent-send-selection-content").action();
    expect(content).toHaveBeenCalledTimes(1);
  });

  it("is disabled with the repository reason before a repository is open", () => {
    expect(find(ready({ currentPath: null }), "agent-send-selection").disabledReason).toContain("Open a repository");
  });

  it("inserts the context tray as references, and says why when it is empty", async () => {
    expect(find(ready(), "agent-insert-tray").disabledReason).toBe("The tray is empty.");
    addToTray("/repo", { checkout: "/repo", path: "a.ts", startLine: 1, endLine: 3 });
    addToTray("/repo", { checkout: "/repo", path: "b.ts", startLine: 7, endLine: 7 });
    const command = find(ready(), "agent-insert-tray");
    expect(command.disabledReason).toBeUndefined();
    expect(command.description).toContain("2 references");
    await command.action();
    expect(sendToAgentFromPage).toHaveBeenCalledWith("@a.ts#L1-L3 @b.ts#L7 ");
  });

  it("keeps another repository's tray out of this one's", () => {
    addToTray("/other", { checkout: "/other", path: "a.ts", startLine: 1, endLine: 1 });
    expect(find(ready(), "agent-insert-tray").disabledReason).toBe("The tray is empty.");
  });
});

describe("palette: the active file", () => {
  it("opens it in a terminal split by asking the panel first and then showing the dock", async () => {
    expect(find(ready(), "file-open-in-terminal-split").disabledReason).toBe("Open a file in Explorer first.");
    const calls: string[] = [];
    const request = vi.spyOn(terminalLaunchRequests, "requestFile").mockImplementation(async () => void calls.push("request"));
    vi.spyOn(repoStore, "setTerminalOpen").mockImplementation(() => { calls.push("dock"); return true; });
    const current = ready({ selectedFilePath: "src/a.ts" });
    vi.spyOn(repoStore, "subscribe").mockImplementation((listener) => { listener(current); return () => {}; });
    await find(current, "file-open-in-terminal-split").action();
    expect(request).toHaveBeenCalledWith("/repo", "src/a.ts", null, null, undefined, { mode: "split", focus: true });
    expect(calls).toEqual(["request", "dock"]);
  });

  it("reveals it in the explorer: Code page, file tree rail, file selected", () => {
    expect(find(ready(), "file-reveal-in-explorer").disabledReason).toBe("Open a file in Explorer first.");
    const setTab = vi.spyOn(repoStore, "setActiveTab").mockImplementation(() => {});
    const select = vi.spyOn(repoStore, "selectFilePath").mockImplementation(() => {});
    const surface = vi.spyOn(interfaceStore, "setGlobalSurface").mockImplementation(() => {});
    void find(ready({ selectedFilePath: "src/a.ts" }), "file-reveal-in-explorer").action();
    expect(surface).toHaveBeenCalledWith("repository");
    expect(setTab).toHaveBeenCalledWith("code");
    expect(select).toHaveBeenCalledWith("src/a.ts");
    expect(get(explorerLens).rail).toBe("files");
  });
});
