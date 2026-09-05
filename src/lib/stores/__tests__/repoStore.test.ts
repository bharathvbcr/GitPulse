import { describe, expect, it, vi, afterEach } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, STATS_DRAIN_MAX_BATCHES, STATS_PUBLISH_EVERY, type BranchInfo, type InvokeFn } from "../repoStore";
import { memoryStorage, STORAGE_KEY_WORKSPACE } from "../../repos/persist";
import { STATUS_POLL_INTERVAL_MS } from "../../repos/statusPoll";
import type { FilterState } from "../filterStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function makeFilter() {
  const store = writable<FilterState>({
    searchQuery: "",
    selectedBranch: null,
  });
  return {
    subscribe: store.subscribe,
    setSearch: (query: string) => store.update((s) => ({ ...s, searchQuery: query })),
    selectBranch: (branch: string | null) => store.update((s) => ({ ...s, selectedBranch: branch })),
    clear: () => store.set({ searchQuery: "", selectedBranch: null }),
  };
}

function makeGraph() {
  const shown: string[] = [];
  const loaded: string[] = [];
  const evicted: string[] = [];
  return {
    shown,
    loaded,
    evicted,
    api: {
      showRepo: (path: string | null) => {
        if (path) shown.push(path);
      },
      loadGraph: async (path: string) => {
        loaded.push(path);
      },
      evict: (path: string) => {
        evicted.push(path);
      },
    },
  };
}

function snapshotFor(path: string) {
  return {
    branches: [
      {
        name: `${path}-main`,
        is_current: true,
        is_remote: false,
        tip_commit_id: "abc",
        ahead_count: 0,
        behind_count: 0,
        is_default: true,
        is_gone: false,
        last_commit_timestamp: 0,
        last_author: "ada",
        last_summary: "init",
        commits_ahead_of_base: 0,
        commits_behind_base: 0,
        additions: 0,
        deletions: 0,
        files_changed: 0,
      },
    ],
    statuses: [{ path: "README.md", status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 }],
    tags: [],
  };
}

function statsFor(path: string, tipCommitId = "abc") {
  return {
    compared_to: `${path}-main`,
    updates: [
      {
        name: `${path}-main`,
        tip_commit_id: tipCommitId,
        is_remote: false,
        remote_name: null,
        additions: 10,
        deletions: 2,
        files_changed: 3,
        commits_ahead_of_base: 4,
        commits_behind_base: 1,
      },
    ],
    computed: 1,
    cached: 0,
    capped: false,
  };
}

function branchFor(name: string, tipCommitId: string, extras: Partial<BranchInfo> = {}): BranchInfo {
  return {
    name,
    is_current: false,
    is_remote: false,
    remote_name: null,
    tip_commit_id: tipCommitId,
    ahead_count: 0,
    behind_count: 0,
    upstream: null,
    is_default: false,
    is_gone: false,
    last_commit_timestamp: 0,
    last_author: "ada",
    last_summary: "init",
    commits_ahead_of_base: 0,
    commits_behind_base: 0,
    additions: 0,
    deletions: 0,
    files_changed: 0,
    ...extras,
  };
}

function churnFor(
  name: string,
  tipCommitId: string,
  additions: number,
  isRemote = false,
  remoteName: string | null = null,
) {
  return {
    name,
    tip_commit_id: tipCommitId,
    is_remote: isRemote,
    remote_name: remoteName,
    additions,
    deletions: additions,
    files_changed: additions,
    commits_ahead_of_base: additions,
    commits_behind_base: 0,
  };
}

function makeInvoke(overrides: Partial<Record<string, InvokeFn>> = {}): InvokeFn {
  const invokeFn: InvokeFn = async (cmd, args) => {
    const override = overrides[cmd];
    if (override) return override(cmd, args);
    if (cmd === "cmd_resolve_repo") {
      const path = String(args?.repoPath ?? "");
      return { path, name: path.split("/").pop(), is_bare: false } as never;
    }
    if (cmd === "cmd_list_branches") {
      return snapshotFor(String(args?.repoPath)).branches as never;
    }
    if (cmd === "cmd_get_status") {
      return snapshotFor(String(args?.repoPath)).statuses as never;
    }
    if (cmd === "cmd_list_tags") return { tags: [], truncated: false } as never;
    // Idle by default; suites that care park an operation via overrides.
    if (cmd === "cmd_repo_operation") return null as never;
    if (cmd === "cmd_stash_list") return [] as never;
    if (cmd === "cmd_branch_stats") return statsFor(String(args?.repoPath)) as never;
    if (cmd === "cmd_watch_repo") return String(args?.repoPath) as never;
    if (cmd === "cmd_unwatch_repo") return undefined as never;
    if (cmd === "cmd_set_recent_menu") return undefined as never;
    if (cmd === "cmd_get_file_diff")
      return { text: `diff ${args?.filePath}`, truncated: false } as never;
    if (cmd === "cmd_get_commit_diff")
      return { text: `commit ${args?.commitId}`, truncated: false } as never;
    throw new Error(`unexpected command ${cmd}`);
  };
  return invokeFn;
}

function makeStore(invoke: InvokeFn = makeInvoke()) {
  const graph = makeGraph();
  const store = createRepoStore({
    invoke,
    storage: memoryStorage(),
    caseInsensitive: true,
    graph: graph.api,
    filter: makeFilter(),
  });
  return { store, graph };
}

describe("repoStore tabs", () => {
  it("opens a new repository on Work, not Graph", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/alpha");
    expect(get(store).activeTab).toBe("work");
  });

  it("opens a second repo as a new tab without inheriting the first selection", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/alpha");
    await store.selectFileDiff("README.md");
    expect(get(store).selectedFilePath).toBe("README.md");
    expect(get(store).selectedIsStaged).toBe(false);
    // Diff is a section of History now, so "landed on the diff" is two facts:
    // the view, and the lens it is showing.
    expect(get(store).activeTab).toBe("history");
    expect(get(store).viewSections.history).toBe("diff");

    await store.openRepo("/r/beta");
    const state = get(store);
    expect(state.openTabs).toHaveLength(2);
    expect(state.currentPath).toBe("/r/beta");
    expect(state.selectedFilePath).toBeNull();
    expect(state.selectedDiff).toBeNull();
    expect(state.activeTab).toBe("work");
    expect(state.currentBranch).toBe("/r/beta-main");
  });

  it("clears the shared file selection when the editor closes its last tab", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/alpha");
    store.selectFilePath("README.md");
    expect(get(store).selectedFilePath).toBe("README.md");

    store.selectFilePath(null);
    expect(get(store).selectedFilePath).toBeNull();
  });

  it("remembers whether the selected worktree diff is the staged side", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/dual");
    await store.selectFileDiff("README.md", true);
    expect(get(store).selectedIsStaged).toBe(true);
    await store.selectFileDiff("README.md", false);
    expect(get(store).selectedIsStaged).toBe(false);
    await store.selectCommitDiff("abc");
    expect(get(store).selectedIsStaged).toBe(false);
    expect(get(store).selectedFilePath).toBeNull();
  });

  it("restores per-tab selection when switching back", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/alpha");
    await store.selectFileDiff("README.md");
    const alphaId = get(store).activeTabId!;
    await store.openRepo("/r/beta");
    await store.activateTab(alphaId);
    const state = get(store);
    expect(state.currentPath).toBe("/r/alpha");
    expect(state.selectedFilePath).toBe("README.md");
    expect(state.activeTab).toBe("history");
    expect(state.viewSections.history).toBe("diff");
    expect(state.selectedDiff).toBe("diff README.md");
  });

  it("drops a stale open when a faster second open of another repo wins the UI", async () => {
    const slow = deferred<{ path: string; name: string; is_bare: boolean }>();
    let resolveCount = 0;
    const invoke = makeInvoke({
      cmd_resolve_repo: async (_cmd, args) => {
        const path = String(args?.repoPath);
        resolveCount += 1;
        if (path === "/r/slow") return slow.promise as never;
        return { path, name: "fast", is_bare: false } as never;
      },
    });
    const { store } = makeStore(invoke);
    const slowOpen = store.openRepo("/r/slow");
    await store.openRepo("/r/fast");
    expect(get(store).currentPath).toBe("/r/fast");
    slow.resolve({ path: "/r/slow", name: "slow", is_bare: false });
    await slowOpen;
    const state = get(store);
    expect(state.openTabs).toHaveLength(2);
    expect(state.currentPath).toBe("/r/fast");
    expect(state.currentBranch).toBe("/r/fast-main");
    expect(resolveCount).toBe(2);
  });

  it("ignores watcher events for repos that are not open", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/alpha");
    await store.handleRepoChanged("/r/not-open");
    expect(get(store).currentPath).toBe("/r/alpha");
  });

  it("closes the active tab and activates its neighbor", async () => {
    const { store, graph } = makeStore();
    await store.openRepo("/r/a");
    await store.openRepo("/r/b");
    await store.openRepo("/r/c");
    await store.closeActiveTab();
    expect(get(store).openTabs.map((tab) => tab.path)).toEqual(["/r/a", "/r/b"]);
    expect(get(store).currentPath).toBe("/r/b");
    expect(graph.evicted).toContain("/r/c");
  });

  it("refuses to open past the tab cap", async () => {
    const { store } = makeStore();
    for (let i = 0; i < 24; i += 1) {
      await store.openRepo(`/r/repo-${i}`);
    }
    await store.openRepo("/r/overflow");
    expect(get(store).openTabs).toHaveLength(24);
    expect(get(store).error ?? "").toMatch(/Too many open repositories/);
  });

  it("keeps a broken restored tab instead of pretending it opened", async () => {
    const invoke = makeInvoke({
      cmd_resolve_repo: async () => {
        throw new Error("Cannot access path");
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/missing/repo", { allowBroken: true });
    const state = get(store);
    expect(state.openTabs).toHaveLength(1);
    expect(state.openTabs[0].error).toMatch(/unavailable|Cannot access/i);
  });

  it("keeps lastClosed when reopen resolve fails", async () => {
    let failB = false;
    const invoke = makeInvoke({
      cmd_resolve_repo: async (_cmd, args) => {
        const path = String(args?.repoPath);
        if (failB && path === "/r/b") throw new Error("missing");
        return { path, name: path.split("/").pop(), is_bare: false } as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/a");
    await store.openRepo("/r/b");
    await store.closeActiveTab();
    expect(get(store).lastClosed[0]).toBe("/r/b");
    failB = true;
    await store.reopenLastClosed();
    expect(get(store).lastClosed[0]).toBe("/r/b");
    expect(get(store).openTabs.map((tab) => tab.path)).toEqual(["/r/a"]);
    expect(get(store).currentPath).toBe("/r/a");
  });

  it("restores persisted tabs, view, and the previously active repo", async () => {
    const storage = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: JSON.stringify({
        version: 1,
        tabs: [
          { path: "/r/one", pinned: false, viewTab: "diff", searchQuery: "feat:", selectedBranch: "main" },
          { path: "/r/two", pinned: true, viewTab: "health", searchQuery: "", selectedBranch: null },
        ],
        activePath: "/r/two",
        recents: ["/r/two", "/r/one"],
        lastClosed: ["/r/old"],
      }),
    });
    const graph = makeGraph();
    const store = createRepoStore({
      invoke: makeInvoke(),
      storage,
      caseInsensitive: true,
      graph: graph.api,
      filter: makeFilter(),
    });
    await store.restoreWorkspace();
    const state = get(store);
    expect(state.openTabs.map((tab) => tab.path)).toEqual(["/r/one", "/r/two"]);
    expect(state.currentPath).toBe("/r/two");
    // `health` is a retired id: it became a section of Insights, and a
    // restored session must land on that section rather than on the view's
    // default pane.
    expect(state.activeTab).toBe("insights");
    expect(state.viewSections.insights).toBe("health");
    expect(state.openTabs.find((tab) => tab.path === "/r/two")?.pinned).toBe(true);
    expect(state.lastClosed).toEqual(["/r/old"]);
    expect(graph.shown[graph.shown.length - 1]).toBe("/r/two");
  });

  it("keeps persisted tab order when the middle tab is the previously active repo", async () => {
    const storage = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: JSON.stringify({
        version: 1,
        tabs: [
          { path: "/r/left", pinned: false, viewTab: "history", searchQuery: "", selectedBranch: null },
          { path: "/r/mid", pinned: false, viewTab: "diff", searchQuery: "wip", selectedBranch: "main" },
          { path: "/r/right", pinned: true, viewTab: "health", searchQuery: "", selectedBranch: null },
        ],
        activePath: "/r/mid",
        recents: ["/r/mid", "/r/right", "/r/left"],
        lastClosed: [],
      }),
    });
    const graph = makeGraph();
    const store = createRepoStore({
      invoke: makeInvoke(),
      storage,
      caseInsensitive: true,
      graph: graph.api,
      filter: makeFilter(),
    });
    await store.restoreWorkspace();
    const state = get(store);
    expect(state.openTabs.map((tab) => tab.path)).toEqual(["/r/left", "/r/mid", "/r/right"]);
    expect(state.currentPath).toBe("/r/mid");
    // The blob was written by a build that still had a Diff tab. Restoring it
    // must land on History showing the diff — not on Work, and not on
    // History's default Graph pane, either of which would look to the user
    // like the app lost where they were.
    expect(state.activeTab).toBe("history");
    expect(state.viewSections.history).toBe("diff");
    expect(state.openTabs[2]?.pinned).toBe(true);
    expect(graph.shown[graph.shown.length - 1]).toBe("/r/mid");
  });

  it("removes a successfully reopened path from lastClosed", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/a");
    await store.openRepo("/r/b");
    await store.closeActiveTab();
    expect(get(store).lastClosed[0]).toBe("/r/b");
    await store.reopenLastClosed();
    expect(get(store).currentPath).toBe("/r/b");
    expect(get(store).lastClosed).not.toContain("/r/b");
  });

  it("never fetches the graph itself: the App effect is the single load owner", async () => {
    const { store, graph } = makeStore();
    await store.openRepo("/r/owner-a");
    await store.openRepo("/r/owner-b");
    await store.activateTab(get(store).openTabs[0].id);
    // openRepo + activateTab used to call graph.loadGraph directly here,
    // doubling every fetch with the App-level effect.
    expect(graph.loaded).toEqual([]);

    // Closing the active tab activates its neighbor: still no store-side
    // fetch, and the neighbor's epoch bumps so the App effect refetches
    // exactly once for it.
    await store.closeActiveTab();
    expect(graph.loaded).toEqual([]);
    expect(graph.shown).toContain("/r/owner-b");
    expect(get(store).currentPath).toBe("/r/owner-b");
  });

  it("hands out strictly rising session generations across opens, activations, and closes", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/gen-a");
    const genA = get(store).generation;
    expect(genA).toBeGreaterThanOrEqual(1);

    // A brand-new tab must not reuse an earlier generation: a restarted
    // counter would let a pre-close in-flight fetch pass the fresh session's
    // guard (F3).
    await store.openRepo("/r/gen-b");
    const genB = get(store).generation;
    expect(genB).toBeGreaterThan(genA);

    await store.activateTab(get(store).openTabs[0].id);
    // The SAME session (/r/gen-a) strictly rises on every activation. It may
    // equal another live session's absolute value — the guard is per session
    // id — but a generation is never handed out twice for one tab id.
    const genA2 = get(store).generation;
    expect(genA2).toBeGreaterThan(genA);

    // /r/gen-a is active; closing it re-activates /r/gen-b on a newer epoch.
    await store.closeActiveTab();
    expect(get(store).currentPath).toBe("/r/gen-b");
    expect(get(store).generation).toBeGreaterThan(genA2);
  });

  it("skips storage writes and recent-menu IPC while only unpersisted state changes", async () => {
    const counts = { setItem: 0, menu: 0 };
    const storage = memoryStorage();
    const baseSetItem = storage.setItem;
    storage.setItem = (key, value) => {
      counts.setItem += 1;
      baseSetItem(key, value);
    };
    const invoke = makeInvoke({
      cmd_set_recent_menu: async () => {
        counts.menu += 1;
        return undefined as never;
      },
    });
    const graph = makeGraph();
    const store = createRepoStore({
      invoke,
      storage,
      caseInsensitive: true,
      graph: graph.api,
      filter: makeFilter(),
    });
    await store.openRepo("/r/persist-a");

    counts.setItem = 0;
    counts.menu = 0;

    await store.setCommitDraft("fix: handle");
    await store.setCommitDraft("fix: handle edge");
    await store.setAmending(true);

    expect(counts.setItem).toBe(0);
    expect(counts.menu).toBe(0);

    await store.setActiveTab("history", "diff");
    expect(counts.setItem).toBeGreaterThan(0);
    const persistedNow = JSON.parse(storage.getItem(STORAGE_KEY_WORKSPACE) ?? "{}") as {
      tabs?: Array<{ path: string; viewTab: string; viewSections?: Record<string, string> }>;
    };
    expect(persistedNow.tabs?.[0]).toMatchObject({
      path: "/r/persist-a",
      viewTab: "history",
      viewSections: { history: "diff" },
    });
  });
});

describe("repoStore persistence debounce", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  function makePersistCountingStore() {
    // Count persisted-write BATCHES via the authoritative workspace key
    // (savePersistedWorkspace also touches legacy keys in the same batch).
    const counts = { setItem: 0, menu: 0 };
    const storage = memoryStorage();
    const baseSetItem = storage.setItem;
    storage.setItem = (key, value) => {
      if (key === STORAGE_KEY_WORKSPACE) counts.setItem += 1;
      baseSetItem(key, value);
    };
    const invoke = makeInvoke({
      cmd_set_recent_menu: async () => {
        counts.menu += 1;
        return undefined as never;
      },
    });
    const filter = makeFilter();
    const store = createRepoStore({
      invoke,
      storage,
      caseInsensitive: true,
      graph: makeGraph().api,
      filter,
    });
    return { store, filter, counts, storage };
  }

  it("coalesces rapid persisted-state changes into one write after the trailing delay", async () => {
    vi.useFakeTimers();
    const { store, filter, counts, storage } = makePersistCountingStore();
    await store.openRepo("/r/debounce");
    counts.setItem = 0;

    // Three keystrokes of a persisted field: no synchronous writes at all.
    filter.setSearch("a");
    filter.setSearch("ab");
    filter.setSearch("abc");
    expect(counts.setItem).toBe(0);

    vi.advanceTimersByTime(300);
    expect(counts.setItem).toBe(1);
    const persistedNow = JSON.parse(storage.getItem(STORAGE_KEY_WORKSPACE) ?? "{}") as {
      tabs?: Array<{ path: string; searchQuery: string }>;
    };
    expect(persistedNow.tabs?.[0]).toMatchObject({ path: "/r/debounce", searchQuery: "abc" });

    // The trailing timer is one-shot: no further writes without new changes.
    vi.advanceTimersByTime(600);
    expect(counts.setItem).toBe(1);
  });

  it("rebuilds the native menu only when recents actually change", async () => {
    vi.useFakeTimers();
    const { store, filter, counts } = makePersistCountingStore();
    await store.openRepo("/r/menu-a");
    await store.openRepo("/r/menu-b");
    counts.menu = 0;

    // Search keystrokes flush with unchanged recents: no menu rebuilds.
    filter.setSearch("x");
    await vi.advanceTimersByTimeAsync(300);
    filter.setSearch("xy");
    await vi.advanceTimersByTimeAsync(300);
    expect(counts.menu).toBe(0);

    store.removeRecent("/r/menu-a");
    await vi.advanceTimersByTimeAsync(300);
    expect(counts.menu).toBe(1);
  });
});

describe("repoStore canonical identity", () => {
  function makeResolvingInvoke(
    overrides: Partial<Record<string, InvokeFn>> = {},
    resolve?: (path: string) => { path: string; name: string; is_bare: boolean },
  ): InvokeFn {
    const base = makeInvoke(overrides);
    return async (cmd, args) => {
      if (!resolve && cmd === "cmd_resolve_repo") {
        const path = String(args?.repoPath ?? "");
        return { path, name: path.split("/").pop(), is_bare: false } as never;
      }
      if (resolve && cmd === "cmd_resolve_repo") {
        return resolve(String(args?.repoPath ?? "")) as never;
      }
      return base(cmd, args);
    };
  }

  it("dedupes tabs when an alias resolves to an already-open canonical repo", async () => {
    const invoke = makeResolvingInvoke({}, (raw) =>
      raw.startsWith("/link/")
        ? { path: "/r/real", name: "real", is_bare: false }
        : { path: raw, name: raw.split("/").pop() ?? raw, is_bare: false },
    );
    const { store } = makeStore(invoke);
    await store.openRepo("/r/real");
    await store.selectFileDiff("README.md");
    const realId = get(store).activeTabId!;
    const realDiff = get(store).selectedDiff;

    // Opening through a symlink alias must land on the existing canonical
    // tab, not spawn a second one for the same physical repository.
    await store.openRepo("/link/alias");
    const state = get(store);
    expect(state.openTabs).toHaveLength(1);
    expect(state.openTabs[0].id).toBe(realId);
    expect(state.openTabs[0].path).toBe("/r/real");
    expect(state.activeTabId).toBe(realId);
    // The canonical tab's own selection survives the alias re-open.
    expect(get(store).selectedDiff).toBe(realDiff);

    // A second alias of the same target dedupes too.
    await store.openRepo("/link/other");
    expect(get(store).openTabs).toHaveLength(1);
  });

  it("subscribes and unsubscribes the watcher under the canonical path", async () => {
    const watched: string[] = [];
    const unwatched: string[] = [];
    const invoke = makeResolvingInvoke(
      {
        cmd_watch_repo: async (_cmd, args) => {
          watched.push(String(args?.repoPath));
          return undefined as never;
        },
        cmd_unwatch_repo: async (_cmd, args) => {
          unwatched.push(String(args?.repoPath));
          return undefined as never;
        },
      },
      (raw) =>
        raw.startsWith("/link/")
          ? { path: "/r/real", name: "real", is_bare: false }
          : { path: raw, name: raw.split("/").pop() ?? raw, is_bare: false },
    );
    const { store } = makeStore(invoke);
    await store.openRepo("/link/repo");
    expect(watched).toEqual(["/r/real"]);

    await store.closeActiveTab();
    expect(unwatched).toEqual(["/r/real"]);
  });

  it("adopts a restored broken alias once it opens successfully, keeping its draft", async () => {
    let ghostResolves = false;
    const invoke = makeResolvingInvoke({}, (raw) => {
      if (raw === "/r/ghost") {
        if (!ghostResolves) throw new Error("Cannot access path");
        return { path: "/r/real", name: "real", is_bare: false };
      }
      return { path: raw, name: raw.split("/").pop() ?? raw, is_bare: false };
    });
    const storage = memoryStorage();
    const graph = makeGraph();
    const store = createRepoStore({
      invoke,
      storage,
      caseInsensitive: true,
      graph: graph.api,
      filter: makeFilter(),
    });

    // Restore-shaped entry: the alias cannot be resolved yet, so the tab
    // stays string-normalized under the raw path.
    await store.openRepo("/r/ghost", { allowBroken: true });
    expect(get(store).openTabs.map((tab) => tab.path)).toEqual(["/r/ghost"]);
    expect(get(store).error ?? "").toMatch(/unavailable|Cannot access/i);
    await store.setCommitDraft("wip notes");

    // The volume comes back; retrying the alias adopts the canonical identity.
    ghostResolves = true;
    await store.openRepo("/r/ghost");
    const state = get(store);
    expect(state.openTabs.map((tab) => tab.path)).toEqual(["/r/real"]);
    expect(state.currentPath).toBe("/r/real");
    expect(state.error).toBeNull();
    // Draft state travels from the alias tab to its canonical successor.
    expect(state.commitDraft).toBe("wip notes");
    // No stale alias entry lingers to resurrect a duplicate tab later.
    expect(state.lastClosed).not.toContain("/r/ghost");

    // Persisted shape is canonical after the first successful open.
    const persisted = JSON.parse(storage.getItem(STORAGE_KEY_WORKSPACE) ?? "{}") as {
      tabs?: Array<{ path: string }>;
      activePath?: string | null;
    };
    expect(persisted.tabs?.map((tab) => tab.path)).toEqual(["/r/real"]);
    expect(persisted.activePath).toBe("/r/real");
  });

  it("keeps the surviving canonical tab's draft over the alias's when merging", async () => {
    let ghostResolves = false;
    const invoke = makeResolvingInvoke({}, (raw) => {
      if (raw === "/r/ghost") {
        if (!ghostResolves) throw new Error("Cannot access path");
        return { path: "/r/real", name: "real", is_bare: false };
      }
      return { path: raw, name: raw.split("/").pop() ?? raw, is_bare: false };
    });
    const { store } = makeStore(invoke);
    // Canonical tab first, with its own draft.
    await store.openRepo("/r/real");
    await store.setCommitDraft("canonical draft");
    const realId = get(store).activeTabId!;

    // Broken alias opened afterwards stays a separate tab until healed.
    await store.openRepo("/r/ghost", { allowBroken: true });
    await store.setCommitDraft("alias draft");
    expect(get(store).openTabs).toHaveLength(2);

    ghostResolves = true;
    await store.openRepo("/r/ghost");
    const state = get(store);
    expect(state.openTabs).toHaveLength(1);
    expect(state.openTabs[0].id).toBe(realId);
    expect(state.commitDraft).toBe("canonical draft");
  });
});

describe("repoStore branch stats", () => {
  const flushMicro = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  it("merges churn from cmd_branch_stats into the loaded branches", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/stats-a");

    await flushMicro();
    const state = get(store);
    expect(state.statsPending).toBe(false);
    expect(state.branches[0]).toMatchObject({
      name: "/r/stats-a-main",
      tip_commit_id: "abc",
      additions: 10,
      deletions: 2,
      files_changed: 3,
      commits_ahead_of_base: 4,
      commits_behind_base: 1,
      compared_to: "/r/stats-a-main",
    });
  });

  it("ignores a churn update whose tip no longer matches the loaded branch", async () => {
    const invoke = makeInvoke({
      cmd_branch_stats: async (_cmd, args) => statsFor(String(args?.repoPath), "old-tip") as never,
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/stale-tip");
    await flushMicro();
    const state = get(store);
    expect(state.branches[0]).toMatchObject({ additions: 0, deletions: 0 });
    expect(state.branches[0].compared_to).toBeUndefined();
    expect(state.statsPending).toBe(false);
  });

  it("discards a stats response that lands after the session generation moved on", async () => {
    const stats = deferred<unknown>();
    const invoke = makeInvoke({ cmd_branch_stats: async () => stats.promise as never });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/race-a");
    expect(get(store).statsPending).toBe(true);

    await store.openRepo("/r/race-b");
    await store.activateTab(get(store).openTabs[0].id);
    stats.resolve(statsFor("/r/race-a"));
    await flushMicro();

    const state = get(store);
    expect(state.currentPath).toBe("/r/race-a");
    expect(state.branches[0].additions).toBe(0);
    expect(state.branches[0].compared_to).toBeUndefined();
    expect(state.statsPending).toBe(false);
  });

  it("runs one stats fetch while refreshes race and merges once it settles", async () => {
    const stats = deferred<unknown>();
    let count = 0;
    const invoke = makeInvoke({
      cmd_branch_stats: async () => {
        count += 1;
        return stats.promise as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/dedupe");

    const first = store.refresh();
    const second = store.refresh();
    await Promise.all([first, second]);
    stats.resolve(statsFor("/r/dedupe"));
    await flushMicro();

    expect(count).toBe(1);
    expect(get(store).branches[0].additions).toBe(10);
    expect(get(store).statsPending).toBe(false);
  });

  it("keeps statsPending true until the fetch settles", async () => {
    const stats = deferred<unknown>();
    const invoke = makeInvoke({ cmd_branch_stats: async () => stats.promise as never });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/pending");

    expect(get(store).statsPending).toBe(true);
    stats.resolve(statsFor("/r/pending"));
    await flushMicro();
    expect(get(store).statsPending).toBe(false);
  });

  it("clears statsPending on failure without surfacing an error", async () => {
    const stats = deferred<never>();
    const invoke = makeInvoke({ cmd_branch_stats: async () => stats.promise as never });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/fail-stats");

    expect(get(store).statsPending).toBe(true);
    stats.reject(new Error("Command not found: cmd_branch_stats"));
    await flushMicro();

    const state = get(store);
    expect(state.statsPending).toBe(false);
    expect(state.error).toBeNull();
    expect(state.branches[0].additions).toBe(0);
    expect(state.branches[0].deletions).toBe(0);
    expect(state.branches[0].compared_to).toBeUndefined();
  });

  it("drains capped reports until capped is false, keeping every batch", async () => {
    const path = "/r/drain";
    const branches = [
      branchFor(`${path}-main`, "abc", { is_current: true }),
      branchFor("feature-a", "tip-a"),
      branchFor("feature-b", "tip-b"),
      branchFor("feature-c", "tip-c"),
    ];
    const batches = [
      { compared_to: `${path}-main`, updates: [churnFor("feature-a", "tip-a", 11)], computed: 1, cached: 0, capped: true },
      { compared_to: `${path}-main`, updates: [churnFor("feature-b", "tip-b", 22)], computed: 1, cached: 0, capped: true },
      { compared_to: `${path}-main`, updates: [churnFor("feature-c", "tip-c", 33)], computed: 1, cached: 0, capped: false },
    ];
    let calls = 0;
    const invoke = makeInvoke({
      cmd_list_branches: async () => branches as never,
      cmd_branch_stats: async () => batches[calls++] as never,
    });
    const { store } = makeStore(invoke);
    await store.openRepo(path);
    await flushMicro();

    expect(calls).toBe(3);
    const names = get(store).branches;
    // Earlier batches must survive later ones: each merges into the CURRENT
    // session branches, not a stale copy.
    expect(names.find((b) => b.name === "feature-a")?.additions).toBe(11);
    expect(names.find((b) => b.name === "feature-b")?.additions).toBe(22);
    expect(names.find((b) => b.name === "feature-c")?.additions).toBe(33);
    expect(get(store).statsPending).toBe(false);
  });

  it("coalesces intermediate stats publishes every 8 batches and on drain", async () => {
    const path = "/r/coalesce";
    const branches = [
      branchFor(`${path}-main`, "abc", { is_current: true }),
      ...Array.from({ length: 16 }, (_, i) => branchFor(`f-${i}`, `tip-${i}`)),
    ];
    let calls = 0;
    const invoke = makeInvoke({
      cmd_list_branches: async () => branches as never,
      cmd_branch_stats: async () => {
        const i = calls++;
        return {
          compared_to: `${path}-main`,
          updates: [churnFor(`f-${i}`, `tip-${i}`, i + 1)],
          computed: 1,
          cached: 0,
          capped: i < 15,
        } as never;
      },
    });
    const { store } = makeStore(invoke);
    const sums: number[] = [];
    store.subscribe((s) => {
      if (s.currentPath !== path) return;
      sums.push(
        s.branches.filter((b) => b.name.startsWith("f-")).reduce((n, b) => n + b.additions, 0)
      );
    });
    await store.openRepo(path);
    await flushMicro();

    expect(calls).toBe(16);
    expect(STATS_PUBLISH_EVERY).toBe(8);
    const last = get(store)
      .branches.filter((b) => b.name.startsWith("f-"))
      .reduce((n, b) => n + b.additions, 0);
    expect(last).toBe(136);
    // Without coalescing the running sum would change on every batch (16
    // distinct totals). Coalescing publishes at batch 8 and 16.
    const distinct = new Set(sums.filter((n) => n > 0));
    expect(distinct.size).toBeLessThan(8);
    expect(distinct.has(136)).toBe(true);
  });

  it("stops draining at the named batch bound instead of looping forever", async () => {
    let calls = 0;
    const invoke = makeInvoke({
      cmd_branch_stats: async () => {
        calls += 1;
        return { compared_to: "x", updates: [], computed: 0, cached: 96, capped: true } as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/bound");
    await flushMicro();

    expect(calls).toBe(STATS_DRAIN_MAX_BATCHES);
    expect(get(store).statsPending).toBe(false);
  });

  it("keeps earlier drain batches and degrades cleanly when a later batch fails", async () => {
    const path = "/r/drain-fail";
    let calls = 0;
    const invoke = makeInvoke({
      cmd_branch_stats: async () => {
        calls += 1;
        if (calls === 1) {
          return {
            compared_to: `${path}-main`,
            updates: [churnFor(`${path}-main`, "abc", 7)],
            computed: 1,
            cached: 0,
            capped: true,
          } as never;
        }
        throw new Error("backend hiccup");
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo(path);
    await flushMicro();

    expect(calls).toBe(2);
    const state = get(store);
    expect(state.branches[0].additions).toBe(7);
    expect(state.statsPending).toBe(false);
    expect(state.error).toBeNull();
  });

  it("applies churn independently when a local branch shares its name with a remote-tracking entry", async () => {
    const path = "/r/clash";
    const branches = [
      // A local branch literally named origin/foo...
      branchFor("origin/foo", "tip-local", { is_current: true }),
      // ...and remote origin's foo, whose update carries the same display name.
      branchFor("origin/foo", "tip-remote", { is_remote: true, remote_name: "origin" }),
    ];
    const invoke = makeInvoke({
      cmd_list_branches: async () => branches as never,
      cmd_branch_stats: async () =>
        ({
          compared_to: `${path}-main`,
          updates: [
            churnFor("origin/foo", "tip-local", 3),
            churnFor("origin/foo", "tip-remote", 9, true, "origin"),
          ],
          computed: 2,
          cached: 0,
          capped: false,
        }) as never,
    });
    const { store } = makeStore(invoke);
    await store.openRepo(path);
    await flushMicro();

    const state = get(store);
    const local = state.branches.find((b) => !b.is_remote && b.name === "origin/foo");
    const remote = state.branches.find((b) => b.is_remote && b.remote_name === "origin");
    expect(local).toMatchObject({ additions: 3 });
    expect(remote).toMatchObject({ additions: 9 });
  });

  it("flags statsFailed on a failed drain and clears it after the next success", async () => {
    const path = "/r/stats-failed";
    let calls = 0;
    const invoke = makeInvoke({
      cmd_branch_stats: async () => {
        calls += 1;
        if (calls === 1) throw new Error("Command not found: cmd_branch_stats");
        return statsFor(path) as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo(path);
    await flushMicro();

    // Failure: pending is over AND the failure marker is lit, together —
    // rows must not read as "still computing" or as real zeros.
    expect(get(store)).toMatchObject({ statsPending: false, statsFailed: true });

    await store.refresh();
    await flushMicro();
    expect(calls).toBe(2);
    expect(get(store)).toMatchObject({ statsPending: false, statsFailed: false });
    expect(get(store).branches[0].additions).toBe(10);
  });

  it("keeps statsFailed clear when a capped drain recovers mid-flight", async () => {
    const path = "/r/drain-recover";
    const branches = [branchFor(`${path}-main`, "abc", { is_current: true }), branchFor("feat", "tip-f")];
    let calls = 0;
    const invoke = makeInvoke({
      cmd_list_branches: async () => branches as never,
      cmd_branch_stats: async () => {
        calls += 1;
        if (calls < 3) {
          return {
            compared_to: `${path}-main`,
            updates: [],
            computed: 0,
            cached: 96,
            capped: true,
          } as never;
        }
        return { compared_to: `${path}-main`, updates: [churnFor("feat", "tip-f", 5)], computed: 1, cached: 0, capped: false } as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo(path);
    await flushMicro();

    expect(calls).toBe(3);
    expect(get(store)).toMatchObject({ statsPending: false, statsFailed: false });
    expect(get(store).branches.find((b) => b.name === "feat")?.additions).toBe(5);
  });

  it("does not mark the active session statsFailed when an orphaned fetch aborts on generation", async () => {
    const stats = deferred<unknown>();
    const invoke = makeInvoke({ cmd_branch_stats: async () => stats.promise as never });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/race-fail");
    expect(get(store).statsPending).toBe(true);

    await store.openRepo("/r/race-other");
    await store.activateTab(get(store).openTabs[0].id);
    stats.reject(new Error("aborted"));
    await flushMicro();

    const state = get(store);
    expect(state.currentPath).toBe("/r/race-fail");
    expect(state.statsPending).toBe(false);
    // The abort is not a failure of THIS session's data; nothing was lost.
    expect(state.statsFailed).toBe(false);
    expect(state.error).toBeNull();
  });
});

describe("repoStore status poll lifecycle", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  function pollSpies() {
    const setSpy = vi.spyOn(globalThis, "setInterval");
    const clearSpy = vi.spyOn(globalThis, "clearInterval");
    return { setSpy, clearSpy };
  }

  it("schedules one interval per workspace, clears it on tab close, restarts lazily", async () => {
    const { setSpy, clearSpy } = pollSpies();
    const { store } = makeStore();
    await store.openRepo("/r/poll-a");
    // Re-opening the same workspace must not stack a second interval.
    await store.openRepo("/r/poll-b");
    expect(setSpy).toHaveBeenCalledTimes(1);
    const handle = setSpy.mock.results[0]?.value;

    await store.closeTab(get(store).openTabs[0].id);
    expect(clearSpy.mock.calls.some(([timer]) => timer === handle)).toBe(true);

    await store.openRepo("/r/poll-c");
    expect(setSpy).toHaveBeenCalledTimes(2);
  });

  it("restarts polling when a tab closes and another remains", async () => {
    const { setSpy, clearSpy } = pollSpies();
    const { store } = makeStore();
    await store.openRepo("/r/keep-a");
    await store.openRepo("/r/keep-b");
    expect(setSpy).toHaveBeenCalledTimes(1);
    const firstHandle = setSpy.mock.results[0]?.value;

    await store.closeTab(get(store).openTabs[0].id);
    expect(clearSpy.mock.calls.some(([timer]) => timer === firstHandle)).toBe(true);
    expect(get(store).openTabs).toHaveLength(1);
    expect(setSpy).toHaveBeenCalledTimes(2);
  });

  it("clears the interval when the workspace is restored", async () => {
    const { setSpy, clearSpy } = pollSpies();
    const { store } = makeStore();
    await store.openRepo("/r/poll-reset");
    expect(setSpy).toHaveBeenCalledTimes(1);

    await store.restoreWorkspace();
    // Reset stops the old interval; reopening the persisted tab restarts one.
    expect(clearSpy).toHaveBeenCalled();
    expect(setSpy).toHaveBeenCalledTimes(2);
  });
});

describe("repoStore watcher coalescing", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  function countingLoads(loaded: Record<string, number>): InvokeFn {
    return makeInvoke({
      cmd_list_branches: async (_cmd, args) => {
        const path = String(args?.repoPath);
        loaded[path] = (loaded[path] ?? 0) + 1;
        return [] as never;
      },
    });
  }

  it("collapses a burst of watcher events into one trailing refresh", async () => {
    vi.useFakeTimers();
    const loaded: Record<string, number> = {};
    const { store } = makeStore(countingLoads(loaded));
    await store.openRepo("/r/storm");
    loaded["/r/storm"] = 0;

    await store.handleRepoChanged("/r/storm");
    await store.handleRepoChanged("/r/storm");
    await store.handleRepoChanged("/r/storm");
    expect(loaded["/r/storm"]).toBe(0);

    await vi.advanceTimersByTimeAsync(199);
    expect(loaded["/r/storm"]).toBe(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(loaded["/r/storm"]).toBe(1);

    // One-shot trailing window: no further refreshes without new events.
    await vi.advanceTimersByTimeAsync(1000);
    expect(loaded["/r/storm"]).toBe(1);
  });

  it("debounces each changed path separately so parallel repos each refresh once", async () => {
    vi.useFakeTimers();
    const loaded: Record<string, number> = {};
    const { store } = makeStore(countingLoads(loaded));
    await store.openRepo("/r/storm-x");
    await store.openRepo("/r/storm-y");
    loaded["/r/storm-x"] = 0;
    loaded["/r/storm-y"] = 0;

    for (const suffix of ["x", "y", "x", "y", "x"]) {
      await store.handleRepoChanged(`/r/storm-${suffix}`);
    }
    await vi.advanceTimersByTimeAsync(200);

    expect(loaded["/r/storm-x"]).toBe(1);
    expect(loaded["/r/storm-y"]).toBe(1);
  });
});

describe("repoStore diff selection", () => {
  const flushMicro = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  const multiFileCommitDiff = [
    "diff --git a/src/a.rs b/src/a.rs",
    "index aaa..bbb 100644",
    "--- a/src/a.rs",
    "+++ b/src/a.rs",
    "@@ -1 +1 @@",
    "-alpha",
    "+ALPHA",
    "diff --git a/src/b.ts b/src/b.ts",
    "index ccc..ddd 100644",
    "--- a/src/b.ts",
    "+++ b/src/b.ts",
    "@@ -1 +1 @@",
    "-beta",
    "+BETA",
  ].join("\n");

  it("selectCommitFileDiff narrows a commit diff to one file and keeps the commit selected", async () => {
    const invoke = makeInvoke({
      cmd_get_commit_file_diff: async (_cmd, args) => {
        if (args?.filePath === "src/b.ts") {
          return {
            text: "diff --git a/src/b.ts b/src/b.ts\n--- a/src/b.ts\n+++ b/src/b.ts\n@@ -1 +1 @@\n-beta\n+BETA",
            truncated: false,
          } as never;
        }
        return { text: "", truncated: false } as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/commit-file");

    await store.selectCommitFileDiff("c1", "src/b.ts");
    const state = get(store);
    expect(state.selectedCommitId).toBe("c1");
    expect(state.selectedFilePath).toBe("src/b.ts");
    expect(state.selectedDiff).toContain("+BETA");
    expect(state.selectedDiff).not.toContain("ALPHA");
    expect(state.activeTab).toBe("history");
    expect(state.viewSections.history).toBe("diff");
  });

  it("selectCommitFileDiff surfaces backend errors without clobbering the selection", async () => {
    const invoke = makeInvoke({
      cmd_get_commit_file_diff: async () => {
        throw new Error("bad object");
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/commit-file-err");

    await store.selectCommitFileDiff("cX", "src/b.ts");
    expect(get(store).error).toMatch(/bad object/);
  });

  it("applies rapid same-tab selections in click order, not resolve order", async () => {
    const slow = deferred<string>();
    let call = 0;
    const invoke = makeInvoke({
      cmd_get_file_diff: async (_cmd, args) => {
        call += 1;
        if (String(args?.filePath) === "slow.txt") return slow.promise as never;
        return { text: `diff ${args?.filePath}`, truncated: false } as never;
      },
    });
    void call;
    const { store } = makeStore(invoke);
    await store.openRepo("/r/race-select");

    const slowSelect = store.selectFileDiff("slow.txt");
    await store.selectFileDiff("fast.txt");
    expect(get(store).selectedFilePath).toBe("fast.txt");

    // The slow first click settles last; it must not overwrite fast.txt.
    slow.resolve("diff slow.txt" as never);
    await slowSelect;
    await flushMicro();
    expect(get(store).selectedFilePath).toBe("fast.txt");
    expect(get(store).selectedDiff).toBe("diff fast.txt");
  });

  it("drops a stale selection error when a newer selection already won", async () => {
    const failing = deferred<string>();
    const invoke = makeInvoke({
      cmd_get_file_diff: async (_cmd, args) =>
        String(args?.filePath) === "boom.txt"
          ? (failing.promise as never)
          : ({ text: `diff ${args?.filePath}`, truncated: false } as never),
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/race-error");

    const doomed = store.selectFileDiff("boom.txt");
    await store.selectFileDiff("ok.txt");
    failing.reject(new Error("too slow"));
    await doomed;
    await flushMicro();

    const state = get(store);
    expect(state.selectedFilePath).toBe("ok.txt");
    expect(state.error ?? "").not.toMatch(/too slow/);
  });

  it("keeps commit/file selections from clobbering each other across kinds", async () => {
    const slowCommit = deferred<string>();
    const invoke = makeInvoke({
      cmd_get_commit_diff: async () => slowCommit.promise as never,
      cmd_get_file_diff: async (_cmd, args) =>
        ({ text: `diff ${args?.filePath}`, truncated: false }) as never,
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/race-mixed");

    const slowSelect = store.selectCommitDiff("slow-commit");
    await store.selectFileDiff("fresh.txt");
    slowCommit.resolve(multiFileCommitDiff as never);
    await slowSelect;
    await flushMicro();

    expect(get(store).selectedFilePath).toBe("fresh.txt");
    expect(get(store).selectedCommitId).toBeNull();
  });

  it("inspectCommitInHistory highlights the SHA on the graph tab without fetching a full diff", async () => {
    const invoke = makeInvoke({
      cmd_get_commit_diff: async () => {
        throw new Error("must not download the full commit");
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/inspect");
    store.inspectCommitInHistory("deadbeef");
    const state = get(store);
    expect(state.activeTab).toBe("history");
    expect(state.selectedCommitId).toBe("deadbeef");
    expect(state.selectedFilePath).toBeNull();
    expect(state.selectedDiff).toBeNull();
  });

  it("stageSelectivePatch routes staging and unstaging to the matching commands", async () => {
    const calls: string[] = [];
    const patch = {
      old_path: "a.ts",
      new_path: "a.ts",
      hunks: [],
    };
    const invoke = makeInvoke({
      cmd_stage_selective_patch: async () => {
        calls.push("stage");
        return undefined as never;
      },
      cmd_unstage_selective_patch: async () => {
        calls.push("unstage");
        return undefined as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/patch");
    await store.stageSelectivePatch(patch, true);
    await store.stageSelectivePatch(patch, false);
    expect(calls).toEqual(["stage", "unstage"]);
  });

  it("stageAll and unstageAll walk the current status lists", async () => {
    const staged: string[] = [];
    const unstaged: string[] = [];
    const invoke = makeInvoke({
      cmd_get_status: async () =>
        [
          { path: "a.ts", status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 },
          { path: "b.ts", status_code: "M", is_staged: true, is_conflicted: false, additions: 1, deletions: 0 },
        ] as never,
      cmd_stage_file: async (_cmd, args) => {
        staged.push(String(args?.filePath));
        return undefined as never;
      },
      cmd_unstage_file: async (_cmd, args) => {
        unstaged.push(String(args?.filePath));
        return undefined as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/batch");
    await store.stageAll();
    expect(staged).toEqual(["a.ts"]);
    await store.unstageAll();
    expect(unstaged).toEqual(["b.ts"]);
  });

  it("quickCommit invokes cmd_quick_commit rather than stage-then-commit", async () => {
    const calls: Array<{ cmd: string; message?: string }> = [];
    const invoke = makeInvoke({
      cmd_quick_commit: async (_cmd, args) => {
        calls.push({ cmd: "cmd_quick_commit", message: String(args?.message) });
        return undefined as never;
      },
      cmd_commit: async (_cmd, args) => {
        calls.push({ cmd: "cmd_commit", message: String(args?.message) });
        return undefined as never;
      },
      cmd_stage_file: async () => {
        calls.push({ cmd: "cmd_stage_file" });
        return undefined as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/quick");
    const outcome = await store.quickCommit("feat: all\n\nbody");
    expect(outcome.ok).toBe(true);
    expect(calls).toEqual([{ cmd: "cmd_quick_commit", message: "feat: all\n\nbody" }]);
  });

  it("quickCommit surfaces a backend refusal without calling cmd_commit", async () => {
    const invoke = makeInvoke({
      cmd_quick_commit: async () => {
        throw new Error("Resolve merge conflicts before committing.");
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/conflicted");
    const outcome = await store.quickCommit("feat: nope");
    expect(outcome.ok).toBe(false);
    expect(outcome.error).toContain("conflict");
    expect(get(store).error).toContain("conflict");
  });
});

describe("repoStore snapshot hydration ordering", () => {
  const flushMicro = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  function statusProbe(statuses: Array<{ path: string }>) {
    return statuses.map((item) => ({
      path: item.path,
      status_code: "M",
      is_staged: false,
      is_conflicted: false,
      additions: 1,
      deletions: 0,
    }));
  }

  it("lets the latest-started snapshot win when two loads share one generation", async () => {
    // activateTab starts a hydrate at generation N; refresh() starts more at
    // the SAME N. The older fetch must not overwrite fresher data when it
    // resolves last (F2).
    const staleStatus = deferred<unknown>();
    let call = 0;
    let armedAt = Number.POSITIVE_INFINITY;
    const invoke = makeInvoke({
      cmd_get_status: async () => {
        call += 1;
        if (call === armedAt) return staleStatus.promise as never;
        return statusProbe([{ path: `probe-${call}` }]) as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/snapshot-race");

    armedAt = call + 1; // the NEXT status fetch hangs
    const staleLoad = store.refresh();
    const freshLoad = store.refresh();
    await freshLoad;
    // Calls so far: 1 = initial hydration, 2 = the hung stale load, 3 = fresh.
    expect(get(store).statuses[0]?.path).toBe("probe-3");

    // The first load settles LAST; its result must be discarded.
    staleStatus.resolve(statusProbe([{ path: "probe-stale" }]));
    await staleLoad;
    await flushMicro();

    const state = get(store);
    expect(state.statuses[0]?.path).toBe("probe-3");
    expect(state.error).toBeNull();
    expect(state.isLoading).toBe(false);
  });

  it("ignores a pre-close hydration that lands after the repo is reopened", async () => {
    const staleStatus = deferred<unknown>();
    let call = 0;
    let armedAt = Number.POSITIVE_INFINITY;
    const invoke = makeInvoke({
      cmd_get_status: async () => {
        call += 1;
        if (call === armedAt) return staleStatus.promise as never;
        return statusProbe([{ path: `fresh-${call}` }]) as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/reincarnated");
    const generationBeforeClose = get(store).generation;

    armedAt = call + 1;
    const hungRefresh = store.refresh();
    await store.closeActiveTab();
    await store.reopenLastClosed();
    expect(get(store).generation).toBeGreaterThan(generationBeforeClose);

    staleStatus.resolve(statusProbe([{ path: "stale-incarnation" }]));
    await hungRefresh;
    await flushMicro();

    const state = get(store);
    expect(state.currentPath).toBe("/r/reincarnated");
    // Calls: 1 = first incarnation, 2 = hung pre-close fetch, 3 = reopen.
    expect(state.statuses[0]?.path).toBe("fresh-3");
    expect(state.error).toBeNull();
  });
});

describe("repoStore status poll publish gating", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("does not republish when a poll returns byte-identical statuses", async () => {
    vi.useFakeTimers();
    let mutate = false;
    const stable = snapshotFor("/r/gate").statuses;
    const invoke = makeInvoke({
      cmd_get_status: async () => {
        // Fresh object identities every call; only field values may differ.
        const copy = stable.map((item) => ({ ...item }));
        if (mutate) copy[0] = { ...copy[0], additions: copy[0].additions + 5 };
        return copy as never;
      },
    });
    const { store } = makeStore(invoke);
    let publishes = 0;
    const unsub = store.subscribe(() => {
      publishes += 1;
    });
    await store.openRepo("/r/gate");
    await vi.advanceTimersByTimeAsync(1);
    const baseline = publishes;

    // Two full ticks of identical payloads: neither may reach subscribers.
    await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS);
    expect(publishes).toBe(baseline);
    await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS);
    expect(publishes).toBe(baseline);

    // A real change still publishes exactly once per tick.
    mutate = true;
    await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS);
    expect(publishes).toBe(baseline + 1);

    unsub();
  });

  it("keeps the statuses array identity stable across gated ticks", async () => {
    vi.useFakeTimers();
    const stable = snapshotFor("/r/gate-identity").statuses;
    const invoke = makeInvoke({
      cmd_get_status: async () => stable.map((item) => ({ ...item })) as never,
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/gate-identity");
    await vi.advanceTimersByTimeAsync(1);
    const before = get(store).statuses;

    await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS * 2);
    expect(get(store).statuses).toBe(before);
  });
});

describe("repoStore ignore-whitespace preference", () => {
  const flushMicro = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  function diffRecordingInvoke(): { invoke: InvokeFn; calls: Array<Record<string, unknown>> } {
    const calls: Array<Record<string, unknown>> = [];
    const invoke = makeInvoke({
      cmd_get_file_diff: async (_cmd, args) => {
        calls.push({ ...(args as Record<string, unknown>) });
        return { text: `diff ${String(args?.filePath)}`, truncated: false } as never;
      },
    });
    return { invoke, calls };
  }

  it("refetches an open worktree diff exactly once carrying ignoreWhitespace", async () => {
    const { invoke, calls } = diffRecordingInvoke();
    const { store } = makeStore(invoke);
    await store.openRepo("/r/ws-file");
    await store.selectFileDiff("README.md");
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ filePath: "README.md", isStaged: false, ignoreWhitespace: false });

    calls.length = 0;
    await store.setIgnoreWhitespace(true);
    expect(get(store).selectedIgnoreWhitespace).toBe(true);
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ filePath: "README.md", isStaged: false, ignoreWhitespace: true });

    // Same-value toggle is a no-op: no refetch, no state churn.
    await store.setIgnoreWhitespace(true);
    expect(calls).toHaveLength(1);

    // The recorded preference rides along on later file clicks.
    await store.selectFileDiff("next.txt", true);
    expect(calls).toHaveLength(2);
    expect(calls[1]).toMatchObject({ filePath: "next.txt", isStaged: true, ignoreWhitespace: true });

    await flushMicro();
    expect(get(store).selectedFilePath).toBe("next.txt");
  });

  it("records the preference for commit-kind selections without fetching any diff", async () => {
    const { invoke, calls } = diffRecordingInvoke();
    const { store } = makeStore(invoke);
    await store.openRepo("/r/ws-commit");
    await store.selectCommitDiff("c1");
    calls.length = 0;

    await store.setIgnoreWhitespace(true);
    expect(get(store).selectedIgnoreWhitespace).toBe(true);
    expect(calls).toHaveLength(0);
    // The commit selection itself stays untouched.
    expect(get(store).selectedCommitId).toBe("c1");

    // ...and the next worktree click inherits it.
    await store.selectFileDiff("later.txt");
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ filePath: "later.txt", ignoreWhitespace: true });
    await flushMicro();
  });

  it("persists selectedIgnoreWhitespace through selectFileDiff without a third argument", async () => {
    const { invoke, calls } = diffRecordingInvoke();
    const { store } = makeStore(invoke);
    await store.openRepo("/r/ws-persist");
    await store.setIgnoreWhitespace(true);
    // One refetch from arming the flag with no selection open yet? None —
    // there was no file-kind selection to refresh.
    expect(calls).toHaveLength(0);

    await store.selectFileDiff("kept.txt", false);
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({ filePath: "kept.txt", ignoreWhitespace: true });
    expect(get(store).selectedIgnoreWhitespace).toBe(true);
  });
});

describe("repoStore post-mutation selection refetch", () => {
  const flushMicro = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
  const patch = { old_path: "a.ts", new_path: "a.ts", hunks: [] };

  it("refetches the open file diff after a stage-patch mutation lands", async () => {
    const diffPaths: string[] = [];
    const invoke = makeInvoke({
      cmd_stage_selective_patch: async () => undefined as never,
      cmd_get_file_diff: async (_cmd, args) => {
        diffPaths.push(String(args?.filePath));
        return { text: `diff ${String(args?.filePath)}`, truncated: false } as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/refetch-stage");
    await store.selectFileDiff("a.ts");
    expect(diffPaths).toEqual(["a.ts"]);

    await store.stageSelectivePatch(patch, true);
    // selectFileDiff after refresh is fire-and-forget inside runMutating.
    await flushMicro();
    expect(diffPaths).toEqual(["a.ts", "a.ts"]);
    expect(get(store).selectedDiff).toContain("diff a.ts");
  });

  it("does not refetch a diff after discard when no file selection is open", async () => {
    let diffCalls = 0;
    const invoke = makeInvoke({
      cmd_discard_changes: async () => undefined as never,
      cmd_get_file_diff: async () => {
        diffCalls += 1;
        return "" as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/refetch-none");
    await store.discardChanges("gone.txt");
    await flushMicro();
    expect(diffCalls).toBe(0);
    expect(get(store).selectedFilePath).toBeNull();
  });

  it("does not refetch for mutations outside the refetch set (checkout)", async () => {
    const diffPaths: string[] = [];
    const invoke = makeInvoke({
      cmd_checkout_branch: async () => undefined as never,
      cmd_get_file_diff: async (_cmd, args) => {
        diffPaths.push(String(args?.filePath));
        return "" as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/refetch-checkout");
    await store.selectFileDiff("a.ts");
    expect(diffPaths).toEqual(["a.ts"]);

    await store.checkoutBranch("feature");
    await flushMicro();
    expect(diffPaths).toEqual(["a.ts"]);
  });
});

describe("repoStore watcher echo suppression", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  function countingLoads(loaded: Record<string, number>): InvokeFn {
    return makeInvoke({
      cmd_list_branches: async (_cmd, args) => {
        const path = String(args?.repoPath);
        loaded[path] = (loaded[path] ?? 0) + 1;
        return [] as never;
      },
      cmd_commit: async () => undefined as never,
    });
  }

  it("drops watcher events inside the post-mutation window and refreshes after it expires", async () => {
    vi.useFakeTimers();
    const loaded: Record<string, number> = {};
    const { store } = makeStore(countingLoads(loaded));
    await store.openRepo("/r/echo");
    await store.commit("feat: echo");
    loaded["/r/echo"] = 0;

    // Echo arrives immediately after the mutation: suppressed entirely — no
    // debounce timer armed, so even waiting past the debounce fires nothing.
    await store.handleRepoChanged("/r/echo");
    await vi.advanceTimersByTimeAsync(400);
    expect(loaded["/r/echo"]).toBe(0);

    // Still inside the 2500ms window.
    await vi.advanceTimersByTimeAsync(2000);
    await store.handleRepoChanged("/r/echo");
    await vi.advanceTimersByTimeAsync(400);
    expect(loaded["/r/echo"]).toBe(0);

    // Window has expired: a genuine external event refreshes again.
    await vi.advanceTimersByTimeAsync(300);
    await store.handleRepoChanged("/r/echo");
    await vi.advanceTimersByTimeAsync(199);
    expect(loaded["/r/echo"]).toBe(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(loaded["/r/echo"]).toBe(1);

    // One-shot window: nothing further without new events.
    await vi.advanceTimersByTimeAsync(1000);
    expect(loaded["/r/echo"]).toBe(1);
  });
});

describe("repoStore background-refresh flicker hardening", () => {
  const flushMicro = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  // The poll-race test below installs fake timers; without this teardown a
  // shuffled run can leak them into unrelated async tests (their setTimeout
  // flushes never fire → 5s hangs).
  afterEach(() => {
    vi.useRealTimers();
  });

  it("does not raise isLoading on a refresh of an already-hydrated session", async () => {
    const hung = deferred<unknown>();
    let call = 0;
    const invoke = makeInvoke({
      cmd_get_status: async () => {
        call += 1;
        if (call === 2) return hung.promise as never; // the background refresh hangs
        return snapshotFor("/r/quiet").statuses as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/quiet");
    expect(get(store).isLoading).toBe(false);

    const settled = store.refresh();
    // While the background refresh is still in flight, the header spinner
    // must stay still: content is already rendered from the first hydrate.
    expect(get(store).isLoading).toBe(false);

    hung.resolve(snapshotFor("/r/quiet").statuses as never);
    await settled;
    await flushMicro();
    expect(get(store).isLoading).toBe(false);
  });

  it("still shows isLoading for a session that never hydrated", async () => {
    const hung = deferred<unknown>();
    let call = 0;
    const invoke = makeInvoke({
      cmd_get_status: async () => {
        call += 1;
        if (call === 1) return hung.promise as never;
        return snapshotFor("/r/first").statuses as never;
      },
    });
    const { store } = makeStore(invoke);
    void store.openRepo("/r/first");
    await flushMicro();
    expect(get(store).isLoading).toBe(true);
    hung.resolve(snapshotFor("/r/first").statuses as never);
    await flushMicro();
    expect(get(store).isLoading).toBe(false);
  });

  it("does not raise isLoading when activating a cached tab", async () => {
    const hung = deferred<unknown>();
    let call = 0;
    const invoke = makeInvoke({
      cmd_get_status: async () => {
        call += 1;
        // Calls: 1 = /r/a hydration, 2 = /r/b hydration, 3 = /r/a's
        // ACTIVATION hydrate — hang that one so the assertion lands
        // mid-activation.
        if (call === 3) return hung.promise as never;
        const path = call === 2 ? "/r/b" : "/r/a";
        return snapshotFor(path).statuses as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/a");
    await store.openRepo("/r/b");
    const activation = store.activateTab(get(store).openTabs[0].id);
    await flushMicro();
    expect(get(store).currentPath).toBe("/r/a");
    expect(get(store).isLoading).toBe(false); // rows are cached; spinner must stay still
    hung.resolve(snapshotFor("/r/a").statuses as never);
    await activation;
  });

  it("stageAll runs exactly one refresh cycle regardless of file count", async () => {
    let branchListings = 0;
    const invoke = makeInvoke({
      cmd_list_branches: async () => {
        branchListings += 1;
        return snapshotFor("/r/bulk").branches as never;
      },
      cmd_get_status: async () =>
        [1, 2, 3].map((n) => ({
          path: `f${n}.ts`,
          status_code: "M",
          is_staged: false,
          is_conflicted: false,
          additions: 1,
          deletions: 0,
        })) as never,
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/bulk"); // hydration listing #1
    await store.stageAll();
    // Pre-fix this was 1 + N (one full refresh per staged file): each cycle
    // flipped the spinner and re-rendered the sidebar N times in a row.
    expect(branchListings).toBe(2);
  });

  it("settles a no-change stats drain without extra publishes", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/noop-stats");
    await flushMicro(); // let the unawaited first stats drain fully settle

    let publishes = 0;
    const unsub = store.subscribe(() => {
      publishes += 1;
    });
    await flushMicro(); // writable fires the subscriber once on subscribe
    const baseline = publishes;
    await store.refresh();
    await flushMicro();
    unsub();

    // Zero delta: the opening patch, hydrate snapshot (churn carried
    // forward), an unchanged stats drain and its settle are ALL no-ops
    // against identical live state — a quiet watcher refresh is invisible.
    expect(publishes - baseline).toBe(0);
  });

  it("discards a poll result that lands after a newer hydration started", async () => {
    vi.useFakeTimers();
    const stalePoll = deferred<unknown>();
    let call = 0;
    let armedAt = Number.POSITIVE_INFINITY;
    const invoke = makeInvoke({
      cmd_get_status: async () => {
        call += 1;
        if (call === armedAt) return stalePoll.promise as never;
        return [{ path: `probe-${call}`, status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 }] as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/poll-race");
    await vi.advanceTimersByTimeAsync(1);

    armedAt = call + 1; // the NEXT status fetch hangs — that one is the poll tick
    await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS);
    const refreshSettled = store.refresh(); // watcher-driven refresh supersedes mid-flight poll
    await vi.advanceTimersByTimeAsync(1);
    await refreshSettled;
    expect(get(store).statuses[0]?.path).not.toBe("probe-stale");

    stalePoll.resolve([{ path: "probe-stale", status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 }] as never);
    await vi.advanceTimersByTimeAsync(1);
    await vi.advanceTimersByTimeAsync(1);

    expect(get(store).statuses[0]?.path).not.toBe("probe-stale");
    expect(get(store).isLoading).toBe(false);
  });
});

describe("repoStore refresh query forwarding", () => {
  interface RecordedLoad {
    path: string;
    query: string;
    revision: string | null;
  }
  function makeRecordingGraph() {
    const loads: RecordedLoad[] = [];
    return {
      loads,
      api: {
        showRepo: (_path: string | null) => {},
        loadGraph: async (path: string, query = "", revision: string | null = null) => {
          loads.push({ path, query, revision });
        },
        evict: (_path: string) => {},
      },
    };
  }

  it("forwards the visible query verbatim to the backend on refresh", async () => {
    const graph = makeRecordingGraph();
    const filter = makeFilter();
    const store = createRepoStore({
      invoke: makeInvoke(),
      storage: memoryStorage(),
      caseInsensitive: true,
      graph: graph.api,
      filter,
    });
    await store.openRepo("/r/launder");

    // Every query term runs in the backend now, and the scheduler keys on
    // the same normalized query the store caches under, so a refresh must
    // reload exactly the view on screen — blanking the query here used to
    // be the only way to keep filtered rows from being laundered into the
    // cache, and that hazard is gone with the client-side filter path.
    filter.setSearch("author:x");
    await store.refresh();
    const authorLoad = graph.loads[graph.loads.length - 1];
    expect(authorLoad).toEqual({ path: "/r/launder", query: "author:x", revision: null });

    filter.selectBranch("feature");
    await store.refresh();
    const branchLoad = graph.loads[graph.loads.length - 1];
    expect(branchLoad?.revision).toBe("feature");

    filter.setSearch("path:src");
    await store.refresh();
    const pathLoad = graph.loads[graph.loads.length - 1];
    expect(pathLoad).toEqual({ path: "/r/launder", query: "path:src", revision: "feature" });
  });
});

describe("repoStore parked operations", () => {
  const mergeOp = {
    kind: "Merge" as const,
    current_step: null,
    total_steps: null,
    head_ref: "main",
    incoming_ref: "Merge branch 'side'",
    conflicted_paths: ["f.txt"],
    conflicted_total: 1,
    available: ["abort" as const],
    warnings: [],
  };

  it("carries the parked operation onto the active session", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_repo_operation: async () => mergeOp as never,
      }),
    );
    await store.openRepo("/r/merging");
    const state = get(store);
    expect(state.operation.probeFailed).toBe(false);
    expect(state.operation.operation?.kind).toBe("Merge");
    expect(state.operation.operation?.conflicted_total).toBe(1);
  });

  it("records a failed probe as unknown rather than as an idle repository", async () => {
    // The dangerous failure: the probe throws, the UI renders a clean repo,
    // and the user acts on a state that was never actually checked.
    const { store } = makeStore(
      makeInvoke({
        cmd_repo_operation: async () => {
          throw new Error("git exploded");
        },
      }),
    );
    await store.openRepo("/r/broken-probe");
    const state = get(store);
    expect(state.operation.probeFailed).toBe(true);
    expect(state.operation.operation).toBeNull();
    // And it must not fail the whole snapshot: branches still rendered.
    expect(state.branches.length).toBeGreaterThan(0);
    expect(state.error).toBeNull();
  });

  it("keeps each repository's operation on its own tab", async () => {
    // A merge parked in one repo must never bleed into another tab's banner.
    const { store } = makeStore(
      makeInvoke({
        cmd_repo_operation: async (_cmd, args) =>
          (String(args?.repoPath) === "/r/parked" ? mergeOp : null) as never,
      }),
    );
    await store.openRepo("/r/parked");
    expect(get(store).operation.operation?.kind).toBe("Merge");

    await store.openRepo("/r/idle");
    expect(get(store).operation.operation).toBeNull();

    await store.activateTab(get(store).openTabs[0].id);
    expect(get(store).operation.operation?.kind).toBe("Merge");
  });

  it("does not republish when an unchanged operation is refetched", async () => {
    // The regression this guards: the snapshot rebuilds the operation object
    // every poll, so reference equality would republish the whole store to
    // every subscriber every six seconds on a repository where nothing moved.
    const { store } = makeStore(
      makeInvoke({
        // A fresh object each call, deliberately — same content, new identity.
        cmd_repo_operation: async () => ({ ...mergeOp, conflicted_paths: ["f.txt"] }) as never,
      }),
    );
    await store.openRepo("/r/quiet");
    let publishes = 0;
    const stop = store.subscribe(() => {
      publishes += 1;
    });
    const baseline = publishes;
    await store.refresh("/r/quiet");
    expect(publishes - baseline).toBe(0);
    stop();
  });

  it("republishes when the operation actually changes", async () => {
    let conflicts = 2;
    const { store } = makeStore(
      makeInvoke({
        cmd_repo_operation: async () =>
          ({ ...mergeOp, conflicted_total: conflicts }) as never,
      }),
    );
    await store.openRepo("/r/moving");
    expect(get(store).operation.operation?.conflicted_total).toBe(2);
    conflicts = 0;
    await store.refresh("/r/moving");
    expect(get(store).operation.operation?.conflicted_total).toBe(0);
  });

  it("sends the action without a kind and refreshes afterwards", async () => {
    // The kind is re-detected by the backend under its lock. Sending a
    // client-side kind would let a stale banner abort the wrong operation.
    const calls: Record<string, unknown>[] = [];
    let parked: unknown = mergeOp;
    const { store } = makeStore(
      makeInvoke({
        cmd_repo_operation: async () => parked as never,
        cmd_repo_operation_action: async (_cmd, args) => {
          calls.push(args ?? {});
          parked = null;
          return { policy: null, output: "aborted" } as never;
        },
      }),
    );
    await store.openRepo("/r/act");
    const outcome = await store.operationAction("abort");
    expect(outcome.ok).toBe(true);
    expect(calls).toHaveLength(1);
    expect(calls[0]).toEqual({ repoPath: "/r/act", action: "abort" });
    expect(calls[0]).not.toHaveProperty("kind");
    // The post-action refresh is what clears the banner.
    expect(get(store).operation.operation).toBeNull();
  });

  it("reports a refused action instead of pretending it ran", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_repo_operation: async () => mergeOp as never,
        cmd_repo_operation_action: async () => {
          throw new Error("Cannot continue this merge while 1 file still has conflict markers");
        },
      }),
    );
    await store.openRepo("/r/refused");
    const outcome = await store.operationAction("continue");
    expect(outcome.ok).toBe(false);
    expect(outcome.error).toContain("conflict markers");
    // The operation is untouched — a refusal is not a state change.
    expect(get(store).operation.operation?.kind).toBe("Merge");
  });
});

describe("repoStore workspace-wide operations", () => {
  const parkedMerge = {
    kind: "Merge" as const,
    current_step: null,
    total_steps: null,
    head_ref: "main",
    incoming_ref: null,
    conflicted_paths: ["f.txt"],
    conflicted_total: 1,
    available: ["abort" as const],
    warnings: [],
  };

  async function openAll(store: ReturnType<typeof makeStore>["store"], paths: string[]) {
    for (const path of paths) await store.openRepo(path);
  }

  it("fetches every open repository and reports a clean sweep", async () => {
    const fetched: string[] = [];
    const { store } = makeStore(
      makeInvoke({
        cmd_fetch: async (_cmd, args) => {
          fetched.push(String(args?.repoPath));
          return "ok" as never;
        },
      }),
    );
    await openAll(store, ["/r/a", "/r/b", "/r/c"]);

    const report = await store.runAcrossOpenRepos("fetch");
    expect(fetched.sort()).toEqual(["/r/a", "/r/b", "/r/c"]);
    expect(report.succeeded).toBe(3);
    expect(report.failed).toBe(0);
    expect(report.skipped).toBe(0);
  });

  it("skips a parked repository and says so rather than counting it fetched", async () => {
    // The honesty property: a repository that was NOT fetched must never be
    // indistinguishable from one that was.
    const fetched: string[] = [];
    const { store } = makeStore(
      makeInvoke({
        cmd_repo_operation: async (_cmd, args) =>
          (String(args?.repoPath) === "/r/parked" ? parkedMerge : null) as never,
        cmd_fetch: async (_cmd, args) => {
          fetched.push(String(args?.repoPath));
          return "ok" as never;
        },
      }),
    );
    await openAll(store, ["/r/clean", "/r/parked"]);

    const report = await store.runAcrossOpenRepos("fetch");
    expect(fetched).toEqual(["/r/clean"]);
    expect(report.succeeded).toBe(1);
    expect(report.skipped).toBe(1);
    const skipped = report.results.find((r) => r.status === "skipped");
    expect(skipped?.path).toBe("/r/parked");
    expect(skipped?.reason).toContain("merge is in progress");
  });

  it("keeps going when one repository's fetch fails", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_fetch: async (_cmd, args) => {
          if (String(args?.repoPath) === "/r/b") throw new Error("remote unreachable");
          return "ok" as never;
        },
      }),
    );
    await openAll(store, ["/r/a", "/r/b", "/r/c"]);

    const report = await store.runAcrossOpenRepos("fetch");
    expect(report.succeeded).toBe(2);
    expect(report.failed).toBe(1);
    expect(report.results.find((r) => r.status === "failed")?.error).toContain(
      "remote unreachable",
    );
  });

  it("pulls rather than fetches when asked", async () => {
    const calls: string[] = [];
    const { store } = makeStore(
      makeInvoke({
        cmd_pull: async () => {
          calls.push("pull");
          return { policy: null, output: "ok" } as never;
        },
        cmd_fetch: async () => {
          calls.push("fetch");
          return "ok" as never;
        },
      }),
    );
    await openAll(store, ["/r/a"]);
    await store.runAcrossOpenRepos("pull");
    expect(calls).toEqual(["pull"]);
  });

  it("reports nothing to do for an empty workspace", async () => {
    const { store } = makeStore();
    const report = await store.runAcrossOpenRepos("fetch");
    expect(report.results).toEqual([]);
  });
});

describe("repoStore workspace work-in-progress", () => {
  it("reports a clean workspace as all clear", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_get_status: async () => [] as never,
      }),
    );
    await store.openRepo("/r/clean");
    const wip = store.workspaceWip();
    expect(wip.examined).toBe(1);
    expect(wip.allClear).toBe(true);
  });

  it("counts uncommitted changes from the live session", async () => {
    // The default mock snapshot carries one modified file.
    const { store } = makeStore();
    await store.openRepo("/r/dirty");
    const wip = store.workspaceWip();
    expect(wip.allClear).toBe(false);
    expect(wip.repos[0].reasons.some((r) => r.kind === "uncommitted")).toBe(true);
  });

  it("counts a stash as work in progress", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_get_status: async () => [] as never,
        cmd_stash_list: async () =>
          [
            {
              index: 0,
              selector: "stash@{0}",
              oid: "abc123",
              subject: "On main: wip",
              message: "wip",
              branch: "main",
              timestamp: 1,
            },
          ] as never,
      }),
    );
    await store.openRepo("/r/stashed");
    const wip = store.workspaceWip();
    expect(wip.repos[0].reasons.some((r) => r.kind === "stash")).toBe(true);
  });

  it("treats a repository whose stash probe failed as unknown, not clean", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_get_status: async () => [] as never,
        cmd_stash_list: async () => {
          throw new Error("git exploded");
        },
      }),
    );
    await store.openRepo("/r/broken");
    const wip = store.workspaceWip();
    expect(wip.allClear).toBe(false);
    expect(wip.unknown).toBe(1);
  });

  it("separates each repository's work", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_get_status: async (_cmd, args) =>
          (String(args?.repoPath) === "/r/dirty"
            ? [
                {
                  path: "a.txt",
                  status_code: "M",
                  is_staged: false,
                  is_conflicted: false,
                  additions: 1,
                  deletions: 0,
                },
              ]
            : []) as never,
      }),
    );
    await store.openRepo("/r/clean");
    await store.openRepo("/r/dirty");
    const wip = store.workspaceWip();
    expect(wip.examined).toBe(2);
    expect(wip.repos).toHaveLength(1);
    expect(wip.repos[0].path).toBe("/r/dirty");
  });
});

describe("repoStore tag list honesty", () => {
  it("records truncation from TagList instead of looking like the whole history", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_list_tags: async () =>
          ({
            tags: [{ name: "v400", commit_id: "abc" }],
            truncated: true,
          }) as never,
      }),
    );
    await store.openRepo("/r/many-tags");
    const state = get(store);
    expect(state.tags).toEqual([{ name: "v400", commit_id: "abc", message: null }]);
    expect(state.tagsTruncated).toBe(true);
    expect(state.tagsFailed).toBe(false);
  });

  it("treats a thrown probe as failed, not as an empty tag list", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_list_tags: async () => {
          throw new Error("git dir is locked");
        },
      }),
    );
    await store.openRepo("/r/locked-tags");
    const state = get(store);
    expect(state.tags).toEqual([]);
    expect(state.tagsFailed).toBe(true);
    expect(state.tagsTruncated).toBe(false);
  });

  it("treats a bare array payload as a failed read, not 'no tags'", async () => {
    // Pre-TagList cmd_list_tags returned Vec<TagInfo>. Accepting that shape
    // as success would revive the silent-empty path the wrapper exists to close.
    const { store } = makeStore(
      makeInvoke({
        cmd_list_tags: async () => [] as never,
      }),
    );
    await store.openRepo("/r/legacy-tags");
    const state = get(store);
    expect(state.tags).toEqual([]);
    expect(state.tagsFailed).toBe(true);
  });
});

describe("repoStore tag actions", () => {
  it("creates a tag through cmd_create_tag with the name and optional commit", async () => {
    const calls: Record<string, unknown>[] = [];
    const { store } = makeStore(
      makeInvoke({
        cmd_create_tag: async (_cmd, args) => {
          calls.push(args ?? {});
          return { policy: null, output: null } as never;
        },
      }),
    );
    await store.openRepo("/r/a");
    const outcome = await store.createTag("v1.0.0", "abc123", "release");
    expect(outcome.ok).toBe(true);
    expect(calls[0]).toEqual({
      repoPath: "/r/a",
      tagName: "v1.0.0",
      commitId: "abc123",
      message: "release",
    });
  });

  it("deletes a tag through cmd_delete_tag", async () => {
    const calls: Record<string, unknown>[] = [];
    const { store } = makeStore(
      makeInvoke({
        cmd_delete_tag: async (_cmd, args) => {
          calls.push(args ?? {});
          return { policy: null, output: null } as never;
        },
      }),
    );
    await store.openRepo("/r/a");
    const outcome = await store.deleteTag("v1.0.0");
    expect(outcome.ok).toBe(true);
    expect(calls[0]).toEqual({ repoPath: "/r/a", tagName: "v1.0.0" });
  });
});

describe("repoStore stash actions", () => {
  const entry = {
    index: 2,
    selector: "stash@{2}",
    oid: "deadbeef",
    subject: "On main: wip",
    message: "wip",
    branch: "main",
    timestamp: 1,
  };

  it("always sends the object id beside the index", async () => {
    // Sending the index alone is what lets a stale list drop the wrong entry.
    const calls: Record<string, unknown>[] = [];
    const { store } = makeStore(
      makeInvoke({
        cmd_stash_action: async (_cmd, args) => {
          calls.push(args ?? {});
          return { policy: null, output: "dropped" } as never;
        },
      }),
    );
    await store.openRepo("/r/a");
    const outcome = await store.stashAction("drop", entry);
    expect(outcome.ok).toBe(true);
    expect(calls[0]).toEqual({
      repoPath: "/r/a",
      action: "drop",
      index: 2,
      expectedOid: "deadbeef",
    });
  });

  it("pops the listed top entry by object id, never stash@{0} sight-unseen", async () => {
    const calls: Record<string, unknown>[] = [];
    const { store } = makeStore(
      makeInvoke({
        cmd_stash_list: async () =>
          [
            {
              index: 0,
              selector: "stash@{0}",
              oid: "abc123",
              subject: "On main: wip",
              message: "wip",
              branch: "main",
              timestamp: 1,
            },
          ] as never,
        cmd_stash_action: async (_cmd, args) => {
          calls.push(args ?? {});
          return { policy: null, output: "Dropped refs/stash@{0}" } as never;
        },
      }),
    );
    await store.openRepo("/r/stashed");
    const outcome = await store.stashPop();
    expect(outcome.ok).toBe(true);
    expect(calls).toEqual([
      { repoPath: "/r/stashed", action: "pop", index: 0, expectedOid: "abc123" },
    ]);
  });

  it("refuses to pop when the stash list could not be read", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_stash_list: async () => {
          throw new Error("git exploded");
        },
        cmd_stash_action: async () => {
          throw new Error("should not be called");
        },
      }),
    );
    await store.openRepo("/r/a");
    const outcome = await store.stashPop();
    expect(outcome.ok).toBe(false);
    expect(outcome.error).toContain("could not be read");
  });

  it("refuses to pop an empty stash rather than targeting stash@{0}", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/a");
    const outcome = await store.stashPop();
    expect(outcome.ok).toBe(false);
    expect(outcome.error).toContain("Nothing is stashed");
  });

  it("surfaces the backend's stale-stack refusal instead of swallowing it", async () => {
    const { store } = makeStore(
      makeInvoke({
        cmd_stash_action: async () => {
          throw new Error(
            "Stash entry 2 changed since it was listed. Refresh the stash list and try again.",
          );
        },
      }),
    );
    await store.openRepo("/r/a");
    const outcome = await store.stashAction("drop", entry);
    expect(outcome.ok).toBe(false);
    expect(outcome.error).toContain("Refresh the stash list");
  });
});

describe("repoStore live-update health", () => {
  it("records a working watcher as live", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/live");
    expect(get(store).watch.status).toBe("watching");
    expect(get(store).watch.reason).toBeNull();
  });

  it("records a failed watch as degraded instead of swallowing it", async () => {
    // The pre-fix behaviour: cmd_watch_repo threw, the catch discarded it, and
    // the repository was indistinguishable from a watched one while its
    // branches, graph and operation state went stale.
    const { store } = makeStore(
      makeInvoke({
        cmd_watch_repo: async () => {
          throw new Error("Too many watched repositories (max 24)");
        },
      }),
    );
    await store.openRepo("/r/unwatched");
    const state = get(store).watch;
    expect(state.status).toBe("degraded");
    expect(state.reason).toContain("Too many watched repositories");
  });

  it("still opens the repository when the watch fails", async () => {
    // A watcher failure must degrade the experience, never block the work.
    const { store } = makeStore(
      makeInvoke({
        cmd_watch_repo: async () => {
          throw new Error("inotify limit reached");
        },
      }),
    );
    const opened = await store.openRepo("/r/unwatched");
    expect(opened).toBe(true);
    expect(get(store).error).toBeNull();
    expect(get(store).branches.length).toBeGreaterThan(0);
  });

  it("polls statuses only while the watcher is healthy", async () => {
    vi.useFakeTimers();
    try {
      const calls: string[] = [];
      const { store } = makeStore(
        makeInvoke({
          cmd_get_status: async (_cmd, args) => {
            calls.push("status");
            return snapshotFor(String(args?.repoPath)).statuses as never;
          },
          cmd_list_branches: async (_cmd, args) => {
            calls.push("branches");
            return snapshotFor(String(args?.repoPath)).branches as never;
          },
        }),
      );
      await store.openRepo("/r/live");
      calls.length = 0;

      await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS + 10);
      expect(calls).toContain("status");
      expect(calls).not.toContain("branches");
    } finally {
      vi.useRealTimers();
    }
  });

  it("upgrades the poll to a full refresh when the watcher is degraded", async () => {
    // The functional half of the fix. Without it the indicator would announce
    // staleness and do nothing about it: the watcher is what refreshes
    // branches, the graph, the operation banner and the stash on an open tab.
    vi.useFakeTimers();
    try {
      const calls: string[] = [];
      const { store } = makeStore(
        makeInvoke({
          cmd_watch_repo: async () => {
            throw new Error("Too many watched repositories (max 24)");
          },
          cmd_get_status: async (_cmd, args) => {
            calls.push("status");
            return snapshotFor(String(args?.repoPath)).statuses as never;
          },
          cmd_list_branches: async (_cmd, args) => {
            calls.push("branches");
            return snapshotFor(String(args?.repoPath)).branches as never;
          },
          cmd_repo_operation: async () => {
            calls.push("operation");
            return null as never;
          },
        }),
      );
      await store.openRepo("/r/unwatched");
      calls.length = 0;

      await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS + 10);
      // A full snapshot: the facets the watcher would have refreshed.
      expect(calls).toContain("branches");
      expect(calls).toContain("operation");
      expect(calls).toContain("status");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("repoStore watch self-healing", () => {
  it("re-asserts the watch periodically so a reaped watcher recovers", async () => {
    // The backend reaps a watch whose event stream closes or whose thread
    // panics, and nothing tells the frontend. Re-asserting repairs it:
    // cmd_watch_repo returns immediately when still registered, and creates a
    // fresh watcher when it was reaped.
    vi.useFakeTimers();
    try {
      let watchCalls = 0;
      const { store } = makeStore(
        makeInvoke({
          cmd_watch_repo: async (_cmd, args) => {
            watchCalls += 1;
            return String(args?.repoPath) as never;
          },
        }),
      );
      await store.openRepo("/r/live");
      expect(watchCalls).toBe(1);

      // Nine ticks: no re-assert yet, so the 6s path stays subprocess-free.
      await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS * 9 + 10);
      expect(watchCalls).toBe(1);

      await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS + 10);
      expect(watchCalls).toBe(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("downgrades to degraded when a re-assert reveals the watch is gone", async () => {
    vi.useFakeTimers();
    try {
      let healthy = true;
      const { store } = makeStore(
        makeInvoke({
          cmd_watch_repo: async (_cmd, args) => {
            if (!healthy) throw new Error("watch session was reaped");
            return String(args?.repoPath) as never;
          },
        }),
      );
      await store.openRepo("/r/live");
      expect(get(store).watch.status).toBe("watching");

      // The watcher dies out from under us.
      healthy = false;
      await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS * 10 + 50);

      const state = get(store).watch;
      expect(state.status).toBe("degraded");
      expect(state.reason).toContain("reaped");
    } finally {
      vi.useRealTimers();
    }
  });

  it("recovers back to live when a later re-assert succeeds", async () => {
    vi.useFakeTimers();
    try {
      let healthy = false;
      const { store } = makeStore(
        makeInvoke({
          cmd_watch_repo: async (_cmd, args) => {
            if (!healthy) throw new Error("watch table full");
            return String(args?.repoPath) as never;
          },
        }),
      );
      await store.openRepo("/r/flaky");
      expect(get(store).watch.status).toBe("degraded");

      healthy = true;
      await vi.advanceTimersByTimeAsync(STATUS_POLL_INTERVAL_MS * 10 + 50);
      expect(get(store).watch.status).toBe("watching");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("repoStore.removeRepo", () => {
  it("removes a closed repository from recents", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/alpha");
    await store.openRepo("/r/beta");
    // Close alpha tab so it becomes recent
    const alphaTab = get(store).openTabs.find((t) => t.path === "/r/alpha");
    expect(alphaTab).toBeDefined();
    await store.closeTab(alphaTab!.id);

    expect(get(store).recentRepos).toContain("/r/alpha");
    expect(get(store).openTabs.map((t) => t.path)).not.toContain("/r/alpha");

    await store.removeRepo("/r/alpha");

    expect(get(store).recentRepos).not.toContain("/r/alpha");
    expect(get(store).openTabs.map((t) => t.path)).not.toContain("/r/alpha");
  });

  it("closes an open tab and removes it from recents and lastClosed", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/alpha");
    await store.openRepo("/r/beta");

    expect(get(store).openTabs).toHaveLength(2);
    expect(get(store).recentRepos).toContain("/r/beta");

    await store.removeRepo("/r/beta");

    const state = get(store);
    expect(state.openTabs.map((t) => t.path)).toEqual(["/r/alpha"]);
    expect(state.recentRepos).not.toContain("/r/beta");
    expect(state.lastClosed).not.toContain("/r/beta");
    expect(state.currentPath).toBe("/r/alpha");
  });

  it("is a no-op for a path not in tabs or recents", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/alpha");
    const countBefore = get(store).openTabs.length;

    await store.removeRepo("/r/non-existent");

    expect(get(store).openTabs).toHaveLength(countBefore);
  });
});

