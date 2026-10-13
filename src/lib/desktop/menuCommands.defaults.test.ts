import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));
vi.mock("../updates/updateCheck", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../updates/updateCheck")>()),
  checkForAppUpdate: vi.fn(),
}));
vi.mock("./openExternal", () => ({ openExternal: vi.fn(async () => {}) }));

const { invoke } = await import("../ipc/invoke");
const { checkForAppUpdate } = await import("../updates/updateCheck");
const { openExternal } = await import("./openExternal");
const { toastStore } = await import("../stores/toastStore");
const { interfaceStore } = await import("../stores/interfaceStore");
const { menuActivity } = await import("./menuStateStore");
const { checkUpdatesFromMenu, defaultMenuCommandDeps } = await import("./menuCommands");

const invokeMock = vi.mocked(invoke);
const checkMock = vi.mocked(checkForAppUpdate);
const openMock = vi.mocked(openExternal);

beforeEach(() => {
  invokeMock.mockReset();
  checkMock.mockReset();
  openMock.mockClear();
  menuActivity.set({});
});

afterEach(() => vi.restoreAllMocks());

describe("the real dependencies behind the native menu", () => {
  it("reads repository state and menu enablement from the stores", () => {
    const deps = defaultMenuCommandDeps();
    expect(deps.state()).toHaveProperty("openTabs");
    // No repository is open in a bare test run, so nothing in the menu is live.
    expect(deps.enabled("fetch")).toBe(false);
    expect(deps.ready("fetch")).toBe(false);
  });

  it("resolves HEAD through the backend and passes any other ref through untouched", async () => {
    const deps = defaultMenuCommandDeps();
    invokeMock.mockResolvedValue("a".repeat(40));
    await expect(deps.commitId("/r", "HEAD")).resolves.toBe("a".repeat(40));
    expect(invokeMock).toHaveBeenCalledWith("cmd_get_head_id", { repoPath: "/r" });
    await expect(deps.commitId("/r", "b".repeat(40))).resolves.toBe("b".repeat(40));
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("raises toasts for errors and successes", () => {
    const deps = defaultMenuCommandDeps();
    const error = vi.spyOn(toastStore, "error").mockImplementation(() => "" as never);
    const success = vi.spyOn(toastStore, "success").mockImplementation(() => "" as never);
    deps.error("boom");
    deps.success("done");
    expect(error).toHaveBeenCalledWith("boom");
    expect(success).toHaveBeenCalledWith("done");
  });

  it("records which command a repository is running, and clears it when it ends", () => {
    const deps = defaultMenuCommandDeps();
    deps.activity("/r", "fetch");
    deps.activity("/other", "pull");
    expect(get(menuActivity)).toEqual({ "/r": ["fetch"], "/other": ["pull"] });
    deps.activity("/r", null);
    expect(get(menuActivity)).toEqual({ "/other": ["pull"] });
  });

  it("sends Quick Commit through the real prompt flow, which refuses with no repository open", async () => {
    const deps = defaultMenuCommandDeps();
    await expect(deps.quickCommit()).resolves.toMatchObject({ ok: false });
  });
});

describe("Check for Updates from the menu", () => {
  const result = (over: Partial<Awaited<ReturnType<typeof checkForAppUpdate>>> = {}) => ({
    currentVersion: "1.0.0",
    latestVersion: "1.1.0",
    updateAvailable: true,
    releaseUrl: "https://example.test/releases/tag/v1.1.0",
    checked: true,
    error: null,
    ...over,
  });

  it("reports a check that could not run as an error, never as up to date", async () => {
    const error = vi.spyOn(toastStore, "error").mockImplementation(() => "" as never);
    const success = vi.spyOn(toastStore, "success").mockImplementation(() => "" as never);
    checkMock.mockResolvedValue(result({ checked: false, updateAvailable: false, error: "offline" }));
    await checkUpdatesFromMenu();
    expect(error).toHaveBeenCalledTimes(1);
    expect(success).not.toHaveBeenCalled();
  });

  it("offers the release and stops nagging about that version", async () => {
    const action = vi.spyOn(toastStore, "action").mockImplementation(() => "" as never);
    const dismiss = vi.spyOn(interfaceStore, "dismissUpdateVersion");
    checkMock.mockResolvedValue(result());
    await checkUpdatesFromMenu();
    expect(dismiss).toHaveBeenCalledWith("1.1.0");
    expect(action).toHaveBeenCalledTimes(1);
    const [, label, onClick, duration] = action.mock.calls[0]!;
    expect(label).toBe("View release");
    expect(duration).toBe(12000);
    (onClick as () => void)();
    expect(openMock).toHaveBeenCalledWith("https://example.test/releases/tag/v1.1.0");
  });

  it("surfaces a failure to open the release page instead of dropping it", async () => {
    const action = vi.spyOn(toastStore, "action").mockImplementation(() => "" as never);
    const error = vi.spyOn(toastStore, "error").mockImplementation(() => "" as never);
    openMock.mockRejectedValueOnce(new Error("no browser"));
    checkMock.mockResolvedValue(result());
    await checkUpdatesFromMenu();
    (action.mock.calls[0]![2] as () => void)();
    await vi.waitFor(() => expect(error).toHaveBeenCalled());
  });

  it("says plainly when the build is current", async () => {
    const success = vi.spyOn(toastStore, "success").mockImplementation(() => "" as never);
    checkMock.mockResolvedValue(result({ updateAvailable: false, latestVersion: "1.0.0" }));
    await checkUpdatesFromMenu();
    expect(success).toHaveBeenCalledTimes(1);
  });
});
