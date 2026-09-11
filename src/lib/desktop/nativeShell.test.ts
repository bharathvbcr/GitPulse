import * as tauriEvent from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { dispatchNativeMenu as _dispatchNativeMenu } from "./nativeActions";
import { subscribeNativeShell, syncRecentMenu, takePendingOpen } from "./nativeShell";

type NativeListener = (event: { payload: unknown }) => unknown;

const dispatchNativeMenu = vi.mocked(_dispatchNativeMenu);
const invokeMock = vi.fn();
const isTauriMock = vi.fn();
const unlistenMenu = vi.fn();
const unlistenOpenRepo = vi.fn();
const unlistenOpenError = vi.fn();
const unlistenDrag = vi.fn();

let menuListener: NativeListener | null = null;
let openRepoListener: NativeListener | null = null;
let openErrorListener: NativeListener | null = null;
let dragDropListener: ((event: { payload: unknown }) => Promise<void> | void) | null = null;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (channel: string, listener: NativeListener) => {
    if (channel === "gitpulse-menu") {
      menuListener = listener;
      return unlistenMenu;
    }
    if (channel === "gitpulse-open-repo") {
      openRepoListener = listener;
      return unlistenOpenRepo;
    }
    if (channel === "gitpulse-open-error") {
      openErrorListener = listener;
      return unlistenOpenError;
    }
    return () => {};
  }),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onDragDropEvent: async (
      listener: (event: { payload: unknown }) => Promise<void> | void,
    ) => {
      dragDropListener = listener;
      return unlistenDrag;
    },
  }),
}));
vi.mock("../platform", () => ({
  isTauri: () => isTauriMock(),
}));
vi.mock("./nativeActions", () => ({
  dispatchNativeMenu: vi.fn(),
}));

function buildHandlers() {
  return {
    canDispatch: vi.fn(() => true),
    activateRepo: vi.fn(),
    clearRecents: vi.fn(),
    checkUpdates: vi.fn(),
    stageAll: vi.fn(),
    unstageAll: vi.fn(),
    createBranch: vi.fn(),
    renameBranch: vi.fn(),
    operationContinue: vi.fn(),
    operationAbort: vi.fn(),
    operationSkip: vi.fn(),
    copyRepoPath: vi.fn(),
    copyBranch: vi.fn(),
    copyCommit: vi.fn(),
    revealRepo: vi.fn(),
    openRemote: vi.fn(),
    open: vi.fn(),
    clone: vi.fn(),
    settings: vi.fn(),
    refresh: vi.fn(),
    toggleTheme: vi.fn(),
    themeSystem: vi.fn(),
    themeLight: vi.fn(),
    themeDark: vi.fn(),
    setTab: vi.fn(),
    shortcuts: vi.fn(),
    diagnostics: vi.fn(),
    documentation: vi.fn(),
    releaseNotes: vi.fn(),
    reportIssue: vi.fn(),
    setupTools: vi.fn(),
    zoomIn: vi.fn(),
    zoomOut: vi.fn(),
    resetZoom: vi.fn(),
    fleet: vi.fn(),
    terminalDock: vi.fn(),
    fetch: vi.fn(),
    pull: vi.fn(),
    push: vi.fn(),
    stash: vi.fn(),
    stashPop: vi.fn(),
    rebase: vi.fn(),
    quickCommit: vi.fn(),
    palette: vi.fn(),
    focusFilter: vi.fn(),
    openRecent: vi.fn(),
    openRepo: vi.fn(),
    closeRepoTab: vi.fn(),
    nextRepoTab: vi.fn(),
    prevRepoTab: vi.fn(),
    reopenRepoTab: vi.fn(),
    openError: vi.fn(),
    setDropActive: vi.fn(),
  };
}

describe("native shell integration", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    dispatchNativeMenu.mockClear();
    invokeMock.mockReset();
    isTauriMock.mockReset().mockReturnValue(true);
    menuListener = null;
    openRepoListener = null;
    openErrorListener = null;
    dragDropListener = null;
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("no-ops all shell commands outside Tauri", async () => {
    isTauriMock.mockReturnValue(false);
    invokeMock.mockResolvedValue("ignored");

    await expect(takePendingOpen()).resolves.toBeNull();
    await expect(syncRecentMenu(["/tmp/repo"])).resolves.toBeUndefined();
    const unsubscribe = await subscribeNativeShell(buildHandlers());
    expect(menuListener).toBeNull();
    expect(dragDropListener).toBeNull();
    expect(invokeMock).not.toHaveBeenCalled();
    expect(unsubscribe).toBeTypeOf("function");
  });

  it("forwards Tauri pending-open failures as a null result", async () => {
    invokeMock.mockRejectedValue(new Error("native offline"));
    await expect(takePendingOpen()).resolves.toBeNull();
    expect(invokeMock).toHaveBeenCalledWith("cmd_take_pending_open");
  });

  it("forwards recent-repo updates to the native command", async () => {
    invokeMock.mockResolvedValue(undefined);
    await syncRecentMenu(["/tmp/repo"]);
    expect(invokeMock).toHaveBeenCalledWith("cmd_set_recent_menu", { paths: ["/tmp/repo"] });
  });

  it("wires native listeners and dispatches shell commands", async () => {
    invokeMock.mockResolvedValue("/repo/actual");
    const handlers = buildHandlers();
    const unsubscribe = await subscribeNativeShell(handlers);

    expect(menuListener).not.toBeNull();
    expect(openRepoListener).not.toBeNull();
    expect(openErrorListener).not.toBeNull();
    expect(dragDropListener).not.toBeNull();

    menuListener?.({ payload: { id: "open" } });
    expect(dispatchNativeMenu).toHaveBeenCalledWith({ id: "open" }, handlers);

    openRepoListener?.({ payload: { path: "/repo/open-repo-path" } });
    expect(handlers.openRepo).toHaveBeenCalledWith("/repo/open-repo-path");

    openErrorListener?.({ payload: "bad command" });
    expect(handlers.openError).toHaveBeenCalledWith("bad command");

    unsubscribe();
    expect(unlistenMenu).toHaveBeenCalledTimes(1);
    expect(unlistenOpenRepo).toHaveBeenCalledTimes(1);
    expect(unlistenOpenError).toHaveBeenCalledTimes(1);
    expect(unlistenDrag).toHaveBeenCalledTimes(1);
  });

  it("resolves dropped paths through root resolution and reports drag-open failures", async () => {
    invokeMock.mockImplementation((command: string, payload?: { path?: string }) => {
      if (command === "cmd_resolve_git_root") {
        if (payload?.path === "/repo/good") return "/root/good";
        throw new Error("not-a-git-repo");
      }
      return "ok";
    });
    const handlers = buildHandlers();
    await subscribeNativeShell(handlers);

    await dragDropListener?.({ payload: { type: "enter", paths: ["/repo/good"] } });
    await dragDropListener?.({ payload: { type: "over" } });
    await dragDropListener?.({ payload: { type: "drop", paths: ["/repo/good"] } });
    expect(handlers.setDropActive).toHaveBeenCalledWith(true);
    expect(handlers.setDropActive).toHaveBeenCalledWith(false);
    expect(handlers.openRepo).toHaveBeenCalledWith("/root/good");

    await dragDropListener?.({ payload: { type: "enter", paths: ["/repo/error"] });
    await dragDropListener?.({ payload: { type: "drop", paths: ["", "/repo/error"] } });
    expect(handlers.openError).toHaveBeenCalledWith("not-a-git-repo");
  });

  it("disposes already-mounted listeners when registration fails", async () => {
    const registrationFailure = new Error("listener setup failed");
    vi.spyOn(tauriEvent, "listen").mockImplementation(async (channel: string, listener: NativeListener) => {
      if (channel === "gitpulse-open-error") throw registrationFailure;
      if (channel === "gitpulse-menu") {
        menuListener = listener;
        return unlistenMenu;
      }
      if (channel === "gitpulse-open-repo") {
        openRepoListener = listener;
        return unlistenOpenRepo;
      }
      return () => {};
    });

    const handlers = buildHandlers();
    await expect(subscribeNativeShell(handlers)).rejects.toBe(registrationFailure);
    expect(unlistenMenu).toHaveBeenCalledTimes(1);
    expect(unlistenOpenRepo).toHaveBeenCalledTimes(1);
    expect(unlistenOpenError).not.toHaveBeenCalled();
    expect(unlistenDrag).not.toHaveBeenCalled();
  });
});
