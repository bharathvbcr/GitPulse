import { beforeEach, describe, expect, it, vi } from "vitest";

const { askConfirmMock } = vi.hoisted(() => ({
  askConfirmMock: vi.fn(async () => true),
}));

vi.mock("../stores/modalStore", () => ({
  askConfirm: askConfirmMock,
}));

import { createCommitDraftStore } from "../stores/commitDraftStore";
import { createSession, createProjectionCache } from "./projection";
import { createSessionBook } from "./sessionBook";
import type { InternalState, RepoSession } from "./storeTypes";
import { createTabActions, openRefusalMessage } from "./tabActions";
import { emptyWorkspace, openTab, setTabGroup } from "./tabModel";
import { sameRepo, type PathIdentityOptions } from "./paths";

const OPTIONS: PathIdentityOptions = { caseInsensitive: false };

function buildHarness(paths: readonly { path: string; group?: string | null }[], terminalCounts: Record<string, number> = {}) {
  let internal: InternalState = {
    workspace: emptyWorkspace(),
    sessions: {},
    workspaceError: null,
  };
  for (const item of paths) {
    const opened = openTab(internal.workspace, item.path, OPTIONS, { activate: true });
    if (!opened.ok) throw new Error(`openTab failed for ${item.path}`);
    let ws = opened.workspace;
    if (item.group) {
      ws = setTabGroup(ws, opened.id, item.group);
    }
    internal = {
      ...internal,
      workspace: ws,
      sessions: {
        ...internal.sessions,
        [opened.id]: createSession({ id: opened.id, path: item.path, pinned: false }),
      },
    };
  }

  const sessionBook = createSessionBook((a, b) => sameRepo(a, b, OPTIONS));
  for (const tab of internal.workspace.tabs) {
    sessionBook.familyUnknown.add(tab.id);
    sessionBook.snapshotRuns.set(tab.id, 1);
  }

  const unwatch = vi.fn(async (_path: string) => {});
  const forgetRepository = vi.fn();
  const stop = vi.fn();
  const ensure = vi.fn();
  const evict = vi.fn();
  const showRepo = vi.fn();
  const syncFilterFromSession = vi.fn();
  const revealGraph = vi.fn();
  const publish = vi.fn();
  const commitEdit = vi.fn();
  const flushPersist = vi.fn();
  let openEpoch = 0;

  const actions = createTabActions({
    invoke: (async () => null) as never,
    storage: null,
    options: OPTIONS,
    graph: {
      showRepo,
      loadGraph: vi.fn(async () => {}),
      evict,
    },
    terminals: {
      countFor: (repoPath) => terminalCounts[repoPath] ?? 0,
    },
    projection: createProjectionCache(),
    sessionBook,
    commitDrafts: createCommitDraftStore(),
    watchCoordinator: {
      noteTabActivated: vi.fn(async () => {}),
      admitWatch: vi.fn(() => ({ admitted: true, evicted: null })),
      hasWatch: vi.fn(() => true),
      acquireWatch: vi.fn(async () => {}),
      unwatch,
      refreshOwedFor: vi.fn(() => false),
      onActivated: vi.fn(),
    },
    hydration: {
      resolvePath: vi.fn(async (p) => ({ path: p, name: p, is_bare: false, common_dir: null })),
      resolvedFields: vi.fn((_id, r) => ({
        path: r.path,
        name: r.name,
        isBare: r.is_bare,
        commonDir: r.common_dir ?? null,
      })),
      snapshotIsFresh: vi.fn(() => false),
      hydrate: vi.fn(async () => {}),
    },
    statusPoller: {
      ensure,
      stop,
      rearm: vi.fn(),
      resetDuration: vi.fn(),
    },
    getInternal: () => internal,
    setWorkspaceError: (workspaceError) => {
      internal = { ...internal, workspaceError };
    },
    setSessions: (sessions) => {
      internal = { ...internal, sessions };
    },
    replaceWorkspace: (workspace) => {
      internal = { ...internal, workspace };
    },
    putSession: (session) => {
      internal = {
        ...internal,
        sessions: { ...internal.sessions, [session.id]: session },
      };
    },
    activeSession: () => {
      const id = internal.workspace.activeId;
      return id ? internal.sessions[id] : undefined;
    },
    applyToSession: (id, generation, patchOrFn) => {
      const live = internal.sessions[id];
      if (!live || live.generation !== generation) return false;
      const patch = typeof patchOrFn === "function" ? patchOrFn(live) : patchOrFn;
      internal = {
        ...internal,
        sessions: { ...internal.sessions, [id]: { ...live, ...patch } },
      };
      return true;
    },
    bumped: (session: RepoSession) => ({
      ...session,
      generation: session.generation + 1,
      statsPending: false,
    }),
    resolveTarget: () => null,
    syncFilterFromSession,
    revealGraph,
    publish,
    holdPublish: () => () => {},
    commitEdit,
    flushPersist,
    suspendPersist: () => () => {},
    resetLoadedWorkspace: () => {},
    getOpenEpoch: () => openEpoch,
    bumpOpenEpoch: () => ++openEpoch,
    setError: vi.fn(),
    forgetRepository,
  });

  return {
    actions,
    getInternal: () => internal,
    sessionBook,
    unwatch,
    forgetRepository,
    stop,
    ensure,
    evict,
    syncFilterFromSession,
    revealGraph,
    publish,
    commitEdit,
    flushPersist,
  };
}

describe("createTabActions", () => {
  beforeEach(() => {
    askConfirmMock.mockReset();
    askConfirmMock.mockResolvedValue(true);
  });

  it("formats open refusal messages consistently", () => {
    expect(openRefusalMessage("invalid")).toBe("Invalid repository path");
    expect(openRefusalMessage("capacity")).toContain("Too many open repositories");
  });

  it("converges closeTab onto shared retireClosedTabs lifecycle", async () => {
    const h = buildHarness([{ path: "/repo/a" }, { path: "/repo/b" }]);
    const secondId = h.getInternal().workspace.tabs[1].id;
    const firstGen = h.getInternal().sessions[h.getInternal().workspace.tabs[0].id].generation;

    await h.actions.closeTab(secondId);

    expect(h.getInternal().workspace.tabs).toHaveLength(1);
    expect(Object.keys(h.getInternal().sessions)).toHaveLength(1);
    expect(h.stop).toHaveBeenCalledTimes(1);
    expect(h.ensure).toHaveBeenCalledTimes(1);
    expect(h.commitEdit).toHaveBeenCalledWith(["/repo/b"]);
    expect(h.unwatch).toHaveBeenCalledWith("/repo/b");
    expect(h.forgetRepository).toHaveBeenCalledWith("/repo/b");
    expect(h.evict).toHaveBeenCalledWith("/repo/b");
    expect(h.getInternal().sessions[h.getInternal().workspace.activeId!].generation).toBe(
      firstGen + 1,
    );
    expect(h.sessionBook.familyUnknown.has(secondId)).toBe(false);
    expect(h.publish).toHaveBeenCalled();
    expect(h.flushPersist).toHaveBeenCalled();
  });

  it("cancels close routes when terminal-loss confirmation is declined", async () => {
    askConfirmMock.mockResolvedValueOnce(false);
    const h = buildHarness(
      [{ path: "/repo/a" }, { path: "/repo/b" }],
      { "/repo/a": 2 },
    );
    const firstId = h.getInternal().workspace.tabs[0].id;

    await h.actions.closeTab(firstId);

    expect(askConfirmMock).toHaveBeenCalledTimes(1);
    expect(h.getInternal().workspace.tabs).toHaveLength(2);
    expect(h.stop).not.toHaveBeenCalled();
    expect(h.unwatch).not.toHaveBeenCalled();
  });

  it("converges closeOtherTabs, closeTabsToTheRight, and closeGroup through retireClosedTabs", async () => {
    const h1 = buildHarness([
      { path: "/repo/a" },
      { path: "/repo/b" },
      { path: "/repo/c" },
    ]);
    const keepId = h1.getInternal().workspace.tabs[1].id;
    await h1.actions.closeOtherTabs(keepId);
    expect(h1.getInternal().workspace.tabs.map((t) => t.path)).toEqual(["/repo/b"]);
    expect(Object.keys(h1.getInternal().sessions)).toEqual([keepId]);
    expect(h1.unwatch).toHaveBeenCalledWith("/repo/a");
    expect(h1.unwatch).toHaveBeenCalledWith("/repo/c");

    const h2 = buildHarness([
      { path: "/repo/a" },
      { path: "/repo/b" },
      { path: "/repo/c" },
    ]);
    await h2.actions.closeTabsToTheRight(h2.getInternal().workspace.tabs[0].id);
    expect(h2.getInternal().workspace.tabs.map((t) => t.path)).toEqual(["/repo/a"]);
    expect(h2.unwatch).toHaveBeenCalledWith("/repo/b");
    expect(h2.unwatch).toHaveBeenCalledWith("/repo/c");

    const h3 = buildHarness([
      { path: "/repo/a", group: "grp" },
      { path: "/repo/b", group: "grp" },
      { path: "/repo/c" },
    ]);
    await h3.actions.closeGroup("grp");
    expect(h3.getInternal().workspace.tabs.map((t) => t.path)).toEqual(["/repo/c"]);
    expect(Object.keys(h3.getInternal().sessions)).toHaveLength(1);
    expect(h3.unwatch).toHaveBeenCalledWith("/repo/a");
    expect(h3.unwatch).toHaveBeenCalledWith("/repo/b");
  });

  describe("a tab opened while the terminal-loss dialog is up", () => {
    type Harness = ReturnType<typeof buildHarness>;

    /** Runs `close` with a dialog held open, opens `/repo/new` behind it, then answers `answer`. */
    async function closeWithTabOpenedBehindDialog(
      h: Harness,
      close: () => Promise<void>,
      answer: boolean,
      opened: { group?: string } = {},
    ): Promise<void> {
      let answerDialog: (yes: boolean) => void = () => {};
      askConfirmMock.mockImplementationOnce(
        () => new Promise<boolean>((resolve) => { answerDialog = resolve; }),
      );
      const closing = close();
      await Promise.resolve();
      expect(askConfirmMock).toHaveBeenCalledTimes(1);
      expect(await h.actions.openRepo("/repo/new", { activate: false, ...opened })).toBe(true);
      answerDialog(answer);
      await closing;
    }

    /** Every tab that left the workspace must have had its watch and graph cache released. */
    function expectNoTabLeftUnreleased(h: Harness, everOpened: readonly string[]): void {
      const open = new Set(h.getInternal().workspace.tabs.map((tab) => tab.path));
      for (const path of everOpened) {
        if (open.has(path)) continue;
        expect(h.unwatch, `${path} left without its watch released`).toHaveBeenCalledWith(path);
        expect(h.evict, `${path} left with its graph cache`).toHaveBeenCalledWith(path);
      }
      expect(Object.keys(h.getInternal().sessions).sort()).toEqual(
        h.getInternal().workspace.tabs.map((tab) => tab.id).sort(),
      );
    }

    const EVER = ["/repo/a", "/repo/b", "/repo/c", "/repo/new"];

    it("closeOtherTabs closes only the tabs the dialog listed and releases each", async () => {
      const h = buildHarness([{ path: "/repo/a" }, { path: "/repo/b" }, { path: "/repo/c" }], { "/repo/b": 1 });
      const keepId = h.getInternal().workspace.tabs[0].id;
      await closeWithTabOpenedBehindDialog(h, () => h.actions.closeOtherTabs(keepId), true);
      expect(h.getInternal().workspace.tabs.map((t) => t.path)).toEqual(["/repo/a", "/repo/new"]);
      expect(h.getInternal().workspace.activeId).toBe(keepId);
      expectNoTabLeftUnreleased(h, EVER);
    });

    it("closeTabsToTheRight closes only the tabs the dialog listed and releases each", async () => {
      const h = buildHarness([{ path: "/repo/a" }, { path: "/repo/b" }, { path: "/repo/c" }], { "/repo/c": 1 });
      const anchorId = h.getInternal().workspace.tabs[0].id;
      await closeWithTabOpenedBehindDialog(h, () => h.actions.closeTabsToTheRight(anchorId), true);
      expect(h.getInternal().workspace.tabs.map((t) => t.path)).toEqual(["/repo/a", "/repo/new"]);
      expect(h.getInternal().workspace.activeId).toBe(anchorId);
      expectNoTabLeftUnreleased(h, EVER);
    });

    it("closeGroup closes only the tabs the dialog listed and releases each", async () => {
      const h = buildHarness(
        [{ path: "/repo/a", group: "grp" }, { path: "/repo/b", group: "grp" }, { path: "/repo/c" }],
        { "/repo/a": 1 },
      );
      await closeWithTabOpenedBehindDialog(h, () => h.actions.closeGroup("grp"), true, { group: "grp" });
      expect(h.getInternal().workspace.tabs.map((t) => t.path)).toEqual(["/repo/c", "/repo/new"]);
      expectNoTabLeftUnreleased(h, EVER);
    });

    it("asks nothing and closes nothing for a tab id that is not open", async () => {
      const h = buildHarness([{ path: "/repo/a" }, { path: "/repo/b" }], { "/repo/a": 1, "/repo/b": 1 });
      await h.actions.closeOtherTabs("no-such-tab");
      await h.actions.closeTabsToTheRight("no-such-tab");
      expect(askConfirmMock).not.toHaveBeenCalled();
      expect(h.getInternal().workspace.tabs).toHaveLength(2);
    });

    it("closes nothing when the dialog is declined", async () => {
      const h = buildHarness([{ path: "/repo/a" }, { path: "/repo/b" }, { path: "/repo/c" }], { "/repo/b": 1 });
      const keepId = h.getInternal().workspace.tabs[0].id;
      await closeWithTabOpenedBehindDialog(h, () => h.actions.closeOtherTabs(keepId), false);
      expect(h.getInternal().workspace.tabs.map((t) => t.path)).toEqual(
        ["/repo/a", "/repo/b", "/repo/c", "/repo/new"],
      );
      expect(h.unwatch).not.toHaveBeenCalled();
    });

    it("skips a tab that was closed behind the dialog and does not close the survivor it was keeping", async () => {
      const h = buildHarness([{ path: "/repo/a" }, { path: "/repo/b" }, { path: "/repo/c" }], { "/repo/b": 1 });
      const [aId, bId] = h.getInternal().workspace.tabs.map((t) => t.id);
      let answerDialog: (yes: boolean) => void = () => {};
      askConfirmMock.mockImplementationOnce(
        () => new Promise<boolean>((resolve) => { answerDialog = resolve; }),
      );
      const closing = h.actions.closeOtherTabs(aId);
      await Promise.resolve();
      await h.actions.closeTab(bId);
      answerDialog(true);
      await closing;
      expect(h.getInternal().workspace.tabs.map((t) => t.path)).toEqual(["/repo/a"]);
      expectNoTabLeftUnreleased(h, ["/repo/a", "/repo/b", "/repo/c"]);
      expect(h.unwatch.mock.calls.filter(([p]) => p === "/repo/b")).toHaveLength(1);
    });

    it("aborts when the tab being kept was closed behind the dialog", async () => {
      const h = buildHarness([{ path: "/repo/a" }, { path: "/repo/b" }, { path: "/repo/c" }], { "/repo/b": 1 });
      const [aId] = h.getInternal().workspace.tabs.map((t) => t.id);
      let answerDialog: (yes: boolean) => void = () => {};
      askConfirmMock.mockImplementationOnce(
        () => new Promise<boolean>((resolve) => { answerDialog = resolve; }),
      );
      const closing = h.actions.closeOtherTabs(aId);
      await Promise.resolve();
      await h.actions.closeTab(aId);
      answerDialog(true);
      await closing;
      expect(h.getInternal().workspace.tabs.map((t) => t.path)).toEqual(["/repo/b", "/repo/c"]);
    });
  });
});
