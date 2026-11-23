import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { repoStore, type RepoState } from "../stores/repoStore";
import { clearActiveSelection, setActiveSelection } from "../terminal/activeSelection";
import { buildCommands } from "./catalog";

const openInPreferredEditor = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock("../desktop/openInEditor", () => ({ openInPreferredEditor }));

const snapshot = (patch: Partial<RepoState> = {}): RepoState => ({ ...get(repoStore), ...patch });
const ready = (patch: Partial<RepoState> = {}) =>
  snapshot({
    activeTabId: "tab-1", currentPath: "/repo", currentBranch: "main", isLoading: false, isBare: false,
    operation: { operation: null, probeFailed: false }, ...patch,
  } as Partial<RepoState>);
const command = (state: RepoState) => {
  const found = buildCommands(state, () => {}).find((item) => item.id === "file-open-in-editor");
  expect(found).toBeDefined();
  return found!;
};
const owner = {};
const select = (path: string, startLine: number) =>
  setActiveSelection(owner, { path, startLine, endLine: startLine + 2, sendReference() {}, sendContent() {}, addToTray() {} });

beforeEach(() => {
  openInPreferredEditor.mockClear();
  vi.spyOn(repoStore, "targetIsActive").mockReturnValue(true);
});
afterEach(() => {
  clearActiveSelection(owner);
  vi.restoreAllMocks();
});

describe("palette: open the active file in the external editor", () => {
  it("is disabled until a repository and a file are open, and says why", () => {
    expect(command(ready({ currentPath: null })).disabledReason).toContain("Open a repository");
    expect(command(ready()).disabledReason).toContain("Open a file in Explorer");
    expect(command(ready({ selectedFilePath: "src/a.ts" })).disabledReason).toBeUndefined();
  });

  it("opens the plain file when the viewer has no selection", async () => {
    await command(ready({ selectedFilePath: "src/a.ts" })).action();
    expect(openInPreferredEditor).toHaveBeenCalledExactlyOnceWith("/repo", "src/a.ts", { line: undefined });
  });

  it("opens at the selection's first line when the selection is in that same file", async () => {
    select("src/a.ts", 42);
    const item = command(ready({ selectedFilePath: "src/a.ts" }));
    expect(item.description).toContain("at line 42");
    await item.action();
    expect(openInPreferredEditor).toHaveBeenCalledExactlyOnceWith("/repo", "src/a.ts", { line: 42 });
  });

  it("ignores a selection that belongs to a different file", async () => {
    select("src/other.ts", 42);
    const item = command(ready({ selectedFilePath: "src/a.ts" }));
    expect(item.description).not.toContain("at line");
    await item.action();
    expect(openInPreferredEditor).toHaveBeenCalledExactlyOnceWith("/repo", "src/a.ts", { line: undefined });
  });

  it("matches the selection's file by repository path, so a case-only difference is the same file on a case-insensitive host", async () => {
    // Vitest runs on a host whose navigator/process platform decides this, as it does in the app.
    const { isCaseInsensitiveFs } = await import("../repos/paths");
    select("SRC/A.ts", 9);
    await command(ready({ selectedFilePath: "src/a.ts" })).action();
    expect(openInPreferredEditor).toHaveBeenCalledExactlyOnceWith("/repo", "src/a.ts", { line: isCaseInsensitiveFs() ? 9 : undefined });
  });

  it("reads the selection when the command runs, not when the palette was built", async () => {
    const item = command(ready({ selectedFilePath: "src/a.ts" }));
    select("src/a.ts", 5);
    await item.action();
    expect(openInPreferredEditor).toHaveBeenCalledExactlyOnceWith("/repo", "src/a.ts", { line: 5 });
  });

  it("refuses to run after the active repository changed", async () => {
    vi.spyOn(repoStore, "targetIsActive").mockReturnValue(false);
    await expect(command(ready({ selectedFilePath: "src/a.ts" })).action()).rejects.toThrow("active repository changed");
    expect(openInPreferredEditor).not.toHaveBeenCalled();
  });

  it("lets the launcher's failure reach the palette, which shows it", async () => {
    openInPreferredEditor.mockRejectedValueOnce(new Error("Zed is not installed"));
    await expect(command(ready({ selectedFilePath: "src/a.ts" })).action()).rejects.toThrow("Zed is not installed");
  });
});
