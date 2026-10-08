/**
 * Open repositories are bounded by what the workspace persists, not by the
 * native watch table: watches are pooled by recency (`repos/watchPool.ts`),
 * and the rest are parked until someone opens them.
 */
import { describe, expect, it } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import { memoryStorage, savePersistedWorkspace, WORKSPACE_VERSION, type StorageLike } from "../../repos/persist";
import { MAX_OPEN_TABS } from "../../repos/tabModel";
import { WATCH_POOL_SIZE } from "../../repos/watchPool";

/** A backend that keeps the native watch table the way the watcher does. */
function backend() {
  const watched = new Set<string>();
  const log: string[] = [];
  let peak = 0;
  const reads: string[] = [];
  const invoke: InvokeFn = async (cmd, args) => {
    const path = String(args?.repoPath ?? "");
    switch (cmd) {
      case "cmd_resolve_repo":
        return { path, name: path.split("/").pop(), is_bare: false, common_dir: `${path}/.git` } as never;
      case "cmd_watch_repo":
        log.push(`watch ${path}`);
        watched.add(path);
        peak = Math.max(peak, watched.size);
        return path as never;
      case "cmd_unwatch_repo":
        log.push(`unwatch ${path}`);
        watched.delete(path);
        return undefined as never;
      case "cmd_get_status":
        reads.push(path);
        return [] as never;
      case "cmd_list_branches":
        return [] as never;
      case "cmd_list_tags":
        return { tags: [], truncated: false } as never;
      case "cmd_repo_operation":
        return null as never;
      case "cmd_stash_list":
        return { entries: [], truncated: false } as never;
      case "cmd_branch_stats":
        return { compared_to: null, updates: [], computed: 0, cached: 0, capped: false } as never;
      default:
        return undefined as never;
    }
  };
  return { invoke, watched, log, reads, peak: () => peak };
}

function makeStore(invoke: InvokeFn, storage: StorageLike = memoryStorage()) {
  return createRepoStore({
    invoke,
    storage,
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
  });
}

const repo = (i: number) => `/code/r${i}`;
const watchOf = (store: ReturnType<typeof makeStore>, path: string) =>
  get(store).openTabs.find((tab) => tab.path === path)!.watch;

describe("more repositories than the watch pool", () => {
  it("opens well past the old 24-tab cap, keeping live watches within the pool", async () => {
    const native = backend();
    const store = makeStore(native.invoke);
    const total = WATCH_POOL_SIZE * 3;
    for (let i = 0; i < total; i += 1) expect(await store.openRepo(repo(i))).toBe(true);
    expect(get(store).openTabs).toHaveLength(total);
    expect(store.openRefusal(repo(total))).toBeNull();
    expect(native.watched.size).toBe(WATCH_POOL_SIZE);
    // Admit-then-release: at most one watch over the pool, transiently.
    expect(native.peak()).toBeLessThanOrEqual(WATCH_POOL_SIZE + 1);
    // The most recent repositories are live; the oldest are parked.
    expect(watchOf(store, repo(total - 1))).toBe("watching");
    expect(watchOf(store, repo(0))).toBe("parked");
    expect(get(store).watch.status).toBe("watching");
  });

  it("brings a parked repository live when it is opened, parking the least recently used", async () => {
    const native = backend();
    const store = makeStore(native.invoke);
    for (let i = 0; i <= WATCH_POOL_SIZE; i += 1) await store.openRepo(repo(i));
    expect(watchOf(store, repo(0))).toBe("parked");
    native.log.length = 0;
    await store.activateTab(get(store).openTabs.find((tab) => tab.path === repo(0))!.id);
    expect(watchOf(store, repo(0))).toBe("watching");
    expect(watchOf(store, repo(1))).toBe("parked");
    expect(native.watched.has(repo(0))).toBe(true);
    expect(native.watched.has(repo(1))).toBe(false);
    // The newcomer's watch is asked for before the evicted one is let go.
    expect(native.log.indexOf(`watch ${repo(0)}`)).toBeLessThan(native.log.indexOf(`unwatch ${repo(1)}`));
    expect(native.watched.size).toBe(WATCH_POOL_SIZE);
  });

  it("never takes a watch from a used repository for a background open", async () => {
    const native = backend();
    const store = makeStore(native.invoke);
    for (let i = 0; i < WATCH_POOL_SIZE; i += 1) await store.openRepo(repo(i));
    native.log.length = 0;
    await store.openRepo("/code/background", { activate: false });
    expect(watchOf(store, "/code/background")).toBe("parked");
    expect(native.log).toEqual([]);
    expect(native.watched.size).toBe(WATCH_POOL_SIZE);
  });

  it("gives a closed repository's slot to the next open", async () => {
    const native = backend();
    const store = makeStore(native.invoke);
    for (let i = 0; i < WATCH_POOL_SIZE; i += 1) await store.openRepo(repo(i));
    await store.closeTab(get(store).openTabs.find((tab) => tab.path === repo(3))!.id);
    expect(native.watched.has(repo(3))).toBe(false);
    await store.openRepo("/code/background", { activate: false });
    expect(watchOf(store, "/code/background")).toBe("watching");
    expect(native.watched.size).toBe(WATCH_POOL_SIZE);
  });

  it("never leaves a native watch behind for a tab closed while its watch was queued", async () => {
    const native = backend();
    const gate: Array<() => void> = [];
    let slow = false;
    const invoke: InvokeFn = async (cmd, args) => {
      if (slow && (cmd === "cmd_watch_repo" || cmd === "cmd_unwatch_repo")) {
        await new Promise<void>((resolve) => gate.push(resolve));
      }
      return native.invoke(cmd, args);
    };
    const store = makeStore(invoke);
    for (let i = 0; i <= WATCH_POOL_SIZE; i += 1) await store.openRepo(repo(i));
    const idOf = (path: string) => get(store).openTabs.find((tab) => tab.path === path)!.id;
    slow = true;
    // r1 holds the oldest slot: activating r0 evicts it, and r1's unwatch
    // queues behind nothing. Then r0 is closed while its own watch waits.
    const activation = store.activateTab(idOf(repo(0)));
    await new Promise((resolve) => setTimeout(resolve, 0));
    const closing = store.closeTab(idOf(repo(0)));
    for (let round = 0; round < 200; round += 1) {
      if (gate.length) gate.shift()!();
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    await Promise.all([activation, closing]);
    expect(get(store).openTabs.some((tab) => tab.path === repo(0))).toBe(false);
    expect(native.watched.has(repo(0))).toBe(false);
  });

  const SHUFFLES = Array.from({ length: 40 }, (_, i) => 0x51ed + i * 7919);
  it.each(SHUFFLES)("keeps watch and unwatch for one repository in the order they were decided (shuffle %i)", async (shuffle) => {
    // Activate a parked tab, and evict it again at once, while the native
    // calls are slow: the last decision must be the one the backend holds.
    const native = backend();
    const gate: Array<() => void> = [];
    let slow = false;
    const invoke: InvokeFn = async (cmd, args) => {
      if (slow && (cmd === "cmd_watch_repo" || cmd === "cmd_unwatch_repo")) {
        await new Promise<void>((resolve) => gate.push(resolve));
      }
      return native.invoke(cmd, args);
    };
    const store = makeStore(invoke);
    for (let i = 0; i <= WATCH_POOL_SIZE; i += 1) await store.openRepo(repo(i));
    const idOf = (path: string) => get(store).openTabs.find((tab) => tab.path === path)!.id;
    slow = true;
    const first = store.activateTab(idOf(repo(0)));
    const churn: Promise<void>[] = [];
    for (let i = 1; i <= WATCH_POOL_SIZE; i += 1) churn.push(store.activateTab(idOf(repo(i))));
    // The backend answers in a shuffled order, not the order it was asked.
    let seed = shuffle;
    const pick = (n: number) => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed % n;
    };
    let settled = false;
    const all = Promise.all([first, ...churn]).then(() => { settled = true; });
    for (let round = 0; round < 5_000 && (!settled || gate.length); round += 1) {
      if (gate.length) gate.splice(pick(gate.length), 1)[0]();
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    await all;
    // Parks are not awaited by an activation; let the last ones land.
    for (let round = 0; round < 200; round += 1) {
      if (gate.length) gate.splice(pick(gate.length), 1)[0]();
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
    expect(gate).toHaveLength(0);
    const live = get(store).openTabs.filter((tab) => tab.watch === "watching").map((tab) => tab.path);
    expect(new Set(native.watched)).toEqual(new Set(live));
    expect(native.watched.size).toBeLessThanOrEqual(WATCH_POOL_SIZE);
  });
});

describe("restoring more repositories than the watch pool", () => {
  it("watches and reads only what the pool holds, active first", async () => {
    const storage = memoryStorage();
    const seed = makeStore(backend().invoke, storage);
    const total = 100;
    for (let i = 0; i < total; i += 1) await seed.openRepo(repo(i), { activate: false });
    await seed.activateTab(get(seed).openTabs.find((tab) => tab.path === repo(60))!.id);
    seed.flushPersistedWorkspace();

    const native = backend();
    const store = makeStore(native.invoke, storage);
    await store.restoreWorkspace();
    expect(get(store).openTabs).toHaveLength(total);
    expect(get(store).currentPath).toBe(repo(60));
    expect(native.watched.size).toBe(WATCH_POOL_SIZE);
    expect(native.log.filter((line) => line.startsWith("watch "))).toHaveLength(WATCH_POOL_SIZE);
    // The active repository is the first one watched and the first one read.
    expect(native.log[0]).toBe(`watch ${repo(60)}`);
    expect(native.reads[0]).toBe(repo(60));
    expect(new Set(native.reads).size).toBe(WATCH_POOL_SIZE);
    const tabs = get(store).openTabs;
    const parked = tabs.filter((tab) => tab.watch === "parked");
    expect(parked).toHaveLength(total - WATCH_POOL_SIZE);
    // A parked tab that was never read does not spin forever, and does not
    // claim to be clean.
    for (const tab of parked) {
      expect(tab.isLoading).toBe(false);
      expect(tab.countsKnown).toBe(false);
    }
  });
});

describe("a restore that replaces live sessions", () => {
  it("gives the restored workspace the whole pool, not the slots of the tabs it replaced", async () => {
    const storage = memoryStorage();
    const seed = makeStore(backend().invoke, storage);
    for (let i = 0; i < 60; i += 1) await seed.openRepo(`/saved/s${i}`, { activate: false });
    seed.flushPersistedWorkspace();

    // A store that already holds a full pool of other repositories.
    const native = backend();
    const store = makeStore(native.invoke, storage);
    for (let i = 0; i < WATCH_POOL_SIZE; i += 1) await store.openRepo(repo(i));
    await store.restoreWorkspace();
    const live = get(store).openTabs.filter((tab) => tab.watch === "watching");
    expect(live).toHaveLength(WATCH_POOL_SIZE);
  });
});

describe("restore cost does not grow with the workspace", () => {
  /** Restores `count` saved tabs and reports what it cost. */
  async function restore(count: number) {
    const storage = memoryStorage();
    savePersistedWorkspace(storage, {
      version: WORKSPACE_VERSION,
      tabs: Array.from({ length: count }, (_, i) => ({
        path: repo(i), pinned: false, viewTab: "work" as const, terminalOpen: false, searchQuery: "", selectedBranch: null,
      })),
      activePath: repo(0),
      recents: [],
      lastClosed: [],
    });
    const native = backend();
    const store = makeStore(native.invoke, storage);
    let publishes = 0;
    const stop = store.subscribe(() => { publishes += 1; });
    await store.restoreWorkspace();
    stop();
    return { store, native, publishes };
  }

  it("publishes as often for a full workspace as for a small one", async () => {
    // Every publish projects every tab, so publishes per tab made a restore
    // quadratic: 500 tabs took 4.5 s. Counted, not timed, so it cannot flake.
    const small = await restore(WATCH_POOL_SIZE * 2);
    const full = await restore(MAX_OPEN_TABS);
    expect(get(full.store).openTabs).toHaveLength(MAX_OPEN_TABS);
    expect(full.publishes).toBe(small.publishes);
    expect(full.native.watched.size).toBe(WATCH_POOL_SIZE);
    expect(new Set(full.native.reads).size).toBe(WATCH_POOL_SIZE);
  }, 30_000);

  it("refuses the tab after the last one, and names the bound", async () => {
    const { store } = await restore(MAX_OPEN_TABS);
    expect(store.openRefusal("/code/one-more")).toBe(
      `Too many open repositories (max ${MAX_OPEN_TABS}). Close a tab to open another.`,
    );
    expect(await store.openRepo("/code/one-more")).toBe(false);
    expect(get(store).openTabs).toHaveLength(MAX_OPEN_TABS);
  }, 30_000);
});
