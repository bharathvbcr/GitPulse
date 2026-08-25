import { describe, expect, it, vi, afterEach } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, STATS_DRAIN_MAX_BATCHES, type BranchInfo, type InvokeFn } from "../repoStore";
import { memoryStorage, STORAGE_KEY_WORKSPACE } from "../../repos/persist";
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
    if (cmd === "cmd_list_tags") return [] as never;
    if (cmd === "cmd_branch_stats") return statsFor(String(args?.repoPath)) as never;
    if (cmd === "cmd_watch_repo") return String(args?.repoPath) as never;
    if (cmd === "cmd_unwatch_repo") return undefined as never;
    if (cmd === "cmd_set_recent_menu") return undefined as never;
    if (cmd === "cmd_get_file_diff") return `diff ${args?.filePath}` as never;
    if (cmd === "cmd_get_commit_diff") return `commit ${args?.commitId}` as never;
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
  it("opens a second repo as a new tab without inheriting the first selection", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/alpha");
    await store.selectFileDiff("README.md");
    expect(get(store).selectedFilePath).toBe("README.md");
    expect(get(store).activeTab).toBe("diff");

    await store.openRepo("/r/beta");
    const state = get(store);
    expect(state.openTabs).toHaveLength(2);
    expect(state.currentPath).toBe("/r/beta");
    expect(state.selectedFilePath).toBeNull();
    expect(state.selectedDiff).toBeNull();
    expect(state.activeTab).toBe("history");
    expect(state.currentBranch).toBe("/r/beta-main");
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
    expect(state.activeTab).toBe("diff");
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
    expect(state.activeTab).toBe("health");
    expect(state.openTabs.find((tab) => tab.path === "/r/two")?.pinned).toBe(true);
    expect(state.lastClosed).toEqual(["/r/old"]);
    expect(graph.shown[graph.shown.length - 1]).toBe("/r/two");
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

  it("bumps the projected activation epoch on open, activate, and activation-by-close", async () => {
    const { store } = makeStore();
    await store.openRepo("/r/gen-a");
    // New session starts at epoch 1.
    expect(get(store).generation).toBe(1);

    await store.openRepo("/r/gen-b");
    expect(get(store).generation).toBe(1);

    await store.activateTab(get(store).openTabs[0].id);
    expect(get(store).generation).toBe(2);

    // /r/gen-a is active; closing it re-activates /r/gen-b (epoch 1 -> 2).
    await store.closeActiveTab();
    expect(get(store).currentPath).toBe("/r/gen-b");
    expect(get(store).generation).toBe(2);
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
    await store.selectFilePath("README.md");

    expect(counts.setItem).toBe(0);
    expect(counts.menu).toBe(0);

    await store.setActiveTab("diff");
    expect(counts.setItem).toBeGreaterThan(0);
    const persistedNow = JSON.parse(storage.getItem(STORAGE_KEY_WORKSPACE) ?? "{}") as {
      tabs?: Array<{ path: string; viewTab: string }>;
    };
    expect(persistedNow.tabs?.[0]).toMatchObject({ path: "/r/persist-a", viewTab: "diff" });
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
