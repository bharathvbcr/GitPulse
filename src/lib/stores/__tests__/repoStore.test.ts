import { describe, expect, it, vi, afterEach } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
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
});

describe("repoStore diff selection races", () => {
  const flushMicro = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  it("applies the LAST REQUEST when two file-diff responses resolve out of order", async () => {
    const slow = deferred<string>();
    const invoke = makeInvoke({
      cmd_get_file_diff: async (_cmd, args) => {
        if (args?.filePath === "slow.txt") return slow.promise as never;
        return `diff ${String(args?.filePath)}` as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/race-file");

    // Request A (slow) first, request B (fast) second. B is the last
    // REQUEST, so A's late resolution must be discarded.
    const firstRequest = store.selectFileDiff("slow.txt");
    const secondRequest = store.selectFileDiff("fast.txt");
    await secondRequest;
    expect(get(store).selectedFilePath).toBe("fast.txt");
    expect(get(store).selectedDiff).toBe("diff fast.txt");

    slow.resolve("diff slow");
    await firstRequest;

    const state = get(store);
    expect(state.selectedFilePath).toBe("fast.txt");
    expect(state.selectedDiff).toBe("diff fast.txt");
    expect(state.error).toBeNull();
  });

  it("applies the last-requested kind: a later commit selection wins over an in-flight file diff", async () => {
    const fileDiff = deferred<string>();
    const invoke = makeInvoke({
      cmd_get_file_diff: async () => fileDiff.promise as never,
      cmd_get_commit_diff: async (_cmd, args) =>
        `commit ${String(args?.commitId)}` as never,
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/race-kind");

    const fileRequest = store.selectFileDiff("slow.txt");
    const commitRequest = store.selectCommitDiff("deadbeef");
    await commitRequest;

    expect(get(store).selectedCommitId).toBe("deadbeef");
    expect(get(store).selectedFilePath).toBeNull();
    expect(get(store).selectedDiff).toBe("commit deadbeef");

    fileDiff.resolve("diff slow");
    await fileRequest;

    const state = get(store);
    expect(state.selectedCommitId).toBe("deadbeef");
    expect(state.selectedDiff).toBe("commit deadbeef");
  });

  it("does not let a stale FAILED diff clobber a newer successful selection", async () => {
    const doomed = deferred<never>();
    let doomedArmed = true;
    const invoke = makeInvoke({
      cmd_get_file_diff: async (_cmd, args) => {
        if (doomedArmed && args?.filePath === "boom.txt") return doomed.promise as never;
        return `diff ${String(args?.filePath)}` as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/race-error");

    const staleRequest = store.selectFileDiff("boom.txt");
    doomedArmed = false;
    await store.selectFileDiff("safe.txt");

    expect(get(store).error).toBeNull();
    doomed.reject(new Error("git blew up"));
    await staleRequest;

    const state = get(store);
    expect(state.selectedFilePath).toBe("safe.txt");
    expect(state.error).toBeNull();
  });

  it("still surfaces the error when the CURRENT request fails", async () => {
    const invoke = makeInvoke({
      cmd_get_commit_diff: async () => {
        throw new Error("no such commit");
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/race-current-error");

    await store.selectCommitDiff("missing");

    expect(get(store).error).toMatch(/no such commit/);
    await flushMicro();
  });

  it("keeps selections from different sessions independent", async () => {
    const alphaDiff = deferred<string>();
    const invoke = makeInvoke({
      cmd_get_file_diff: async (_cmd, args) => {
        if (args?.repoPath === "/r/sel-a" && args?.filePath === "a-slow.txt") {
          return alphaDiff.promise as never;
        }
        return `diff ${String(args?.filePath)} on ${String(args?.repoPath)}` as never;
      },
    });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/sel-a");
    const alphaId = get(store).activeTabId!;
    const staleAlpha = store.selectFileDiff("a-slow.txt");
    await store.openRepo("/r/sel-b");
    await store.selectFileDiff("b.txt");
    await store.activateTab(alphaId);

    // The stale /r/sel-a response lands after reactivation; the fresh
    // generation guard plus seq must not let it paint over the reactivated
    // session's own (empty) pane.
    alphaDiff.resolve("diff a-stale");
    await staleAlpha;
    expect(get(store).selectedFilePath).toBeNull();
    expect(get(store).selectedDiff).toBeNull();
  });
});

describe("repoStore mutation progress", () => {
  const flushMicro = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  it("exposes a human pendingMutation label while a mutation runs and clears it after", async () => {
    const op = deferred<unknown>();
    const invoke = makeInvoke({ cmd_pull: async () => op.promise as never });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/mutate-run");
    expect(get(store).pendingMutation).toBeNull();

    const running = store.pull();
    await flushMicro();
    expect(get(store).pendingMutation).toBe("Pulling");

    op.resolve(undefined);
    const outcome = await running;
    expect(outcome.ok).toBe(true);
    expect(get(store).pendingMutation).toBeNull();
  });

  it("clears pendingMutation through finally even when the mutation fails", async () => {
    const op = deferred<never>();
    const invoke = makeInvoke({ cmd_push: async () => op.promise as never });
    const { store } = makeStore(invoke);
    await store.openRepo("/r/mutate-fail");

    const running = store.push();
    await flushMicro();
    expect(get(store).pendingMutation).toBe("Pushing");

    op.reject(new Error("rejected by remote"));
    const outcome = await running;
    expect(outcome.ok).toBe(false);
    expect(get(store).pendingMutation).toBeNull();
  });

  it("clears pendingMutation when no repository is open (early return path)", async () => {
    const { store } = makeStore();
    const outcome = await store.pull();
    expect(outcome.ok).toBe(false);
    expect(get(store).pendingMutation).toBeNull();
  });
});

describe("repoStore workspace restore concurrency", () => {
  function makeTrackedInvoke() {
    const events: string[] = [];
    let inflight = 0;
    let maxInflight = 0;
    const invoke = makeInvoke({
      cmd_resolve_repo: async (_cmd, args) => {
        const path = String(args?.repoPath);
        inflight += 1;
        maxInflight = Math.max(maxInflight, inflight);
        events.push(`start:${path}`);
        await new Promise((resolve) => setTimeout(resolve, 1));
        inflight -= 1;
        events.push(`end:${path}`);
        return { path, name: path.split("/").pop(), is_bare: false } as never;
      },
    });
    return { invoke, events, maxInflight: () => maxInflight };
  }

  it("restores the active repo FIRST and drains the rest with bounded parallelism of 3", async () => {
    const tracked = makeTrackedInvoke();
    const storage = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: JSON.stringify({
        version: 1,
        tabs: [
          { path: "/r/tab-1", pinned: false, viewTab: "history", searchQuery: "", selectedBranch: null },
          { path: "/r/tab-2", pinned: false, viewTab: "history", searchQuery: "", selectedBranch: null },
          { path: "/r/tab-3", pinned: false, viewTab: "history", searchQuery: "", selectedBranch: null },
          { path: "/r/tab-4", pinned: false, viewTab: "history", searchQuery: "", selectedBranch: null },
          { path: "/r/tab-5", pinned: false, viewTab: "history", searchQuery: "", selectedBranch: null },
          { path: "/r/tab-6", pinned: false, viewTab: "history", searchQuery: "", selectedBranch: null },
          { path: "/r/tab-7", pinned: false, viewTab: "history", searchQuery: "", selectedBranch: null },
        ],
        activePath: "/r/tab-4",
        recents: [],
        lastClosed: [],
      }),
    });
    const graph = makeGraph();
    const store = createRepoStore({
      invoke: tracked.invoke,
      storage,
      caseInsensitive: true,
      graph: graph.api,
      filter: makeFilter(),
    });

    await store.restoreWorkspace();

    // The previously-active repo starts hydrating before anything else.
    expect(tracked.events[0]).toBe("start:/r/tab-4");
    // Seven tabs: head alone, then batches of 3+3 — never more than 3
    // resolves in flight.
    expect(tracked.maxInflight()).toBe(3);
    expect(tracked.events.filter((event) => event.startsWith("start:"))).toHaveLength(7);

    // The user's saved tab arrangement survives the active-first open.
    const state = get(store);
    expect(state.openTabs.map((tab) => tab.path)).toEqual([
      "/r/tab-1",
      "/r/tab-2",
      "/r/tab-3",
      "/r/tab-4",
      "/r/tab-5",
      "/r/tab-6",
      "/r/tab-7",
    ]);
    expect(state.currentPath).toBe("/r/tab-4");
  });
});

describe("commit diff payload plumbing", () => {
  it("stores the structured truncated payload from cmd_get_commit_diff as-is", async () => {
    const payload = {
      content: "diff --git a/a.txt b/a.txt\n+++ a.txt\n",
      truncated: true,
      included_files: 2,
      skipped_files: [{ path: "big.min.js", additions: 9001, deletions: 0 }],
      total_files: 3,
      total_additions: 9100,
      total_deletions: 12,
    };
    const invoke: InvokeFn = (cmd, args) => {
      if (cmd === "cmd_open_repo") {
        return Promise.resolve({
          path: args?.repoPath ?? "/r/payload",
          name: "payload",
          is_bare: false,
        } as never);
      }
      if (cmd === "cmd_get_commit_diff") {
        return Promise.resolve(payload as never);
      }
      return makeInvoke()(cmd, args);
    };
    const { store } = makeStore(invoke);
    await store.openRepo("/r/payload");
    await store.selectCommitDiff("abc1234");
    const state = get(store);
    expect(state.selectedCommitId).toBe("abc1234");
    expect(state.selectedDiff).not.toBeNull();
    expect(typeof state.selectedDiff).toBe("object");
    const diff = state.selectedDiff as { truncated?: boolean; total_files?: number };
    expect(diff.truncated).toBe(true);
    expect(diff.total_files).toBe(3);

    // A legacy bare-string response still lands unchanged for downstream
    // normalization.
    const legacyInvoke: InvokeFn = (cmd, args) => {
      if (cmd === "cmd_get_commit_diff") return Promise.resolve("legacy raw patch" as never);
      return invoke(cmd, args);
    };
    const legacy = makeStore(legacyInvoke);
    await legacy.store.openRepo("/r/payload");
    await legacy.store.selectCommitDiff("def5678");
    expect(get(legacy.store).selectedDiff).toBe("legacy raw patch");
  });
});
