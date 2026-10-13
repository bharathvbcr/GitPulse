/**
 * Opening and closing repositories must leave nothing behind, by any route.
 *
 * A session that lives for days opens and closes repositories without bound,
 * so any record keyed by tab id or path that survives its tab grows for the
 * life of the app. This drives the real store (mocked IPC) through open,
 * mutate, journal, switch-view and close for many repositories, through every
 * close route, and requires the store's retained records, the harness
 * journal cache, the graph cache and the live timer count to return to what
 * they were before the first open. The assertions are counts, not time, so a
 * loaded runner cannot flake them.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import { harnessStore } from "../harnessStore";
import { memoryStorage } from "../../repos/persist";

const invoke: InvokeFn = async (cmd, args) => {
  const path = String(args?.repoPath ?? "");
  switch (cmd) {
    case "cmd_resolve_repo":
      return { path, name: path.split("/").pop(), is_bare: false, common_dir: `${path}/.git` } as never;
    case "cmd_watch_repo":
      return path as never;
    case "cmd_list_tags":
      return { tags: [], truncated: false } as never;
    case "cmd_stash_list":
      return { entries: [], truncated: false } as never;
    case "cmd_branch_stats":
      return { compared_to: null, updates: [], computed: 0, cached: 0, capped: false } as never;
    case "cmd_get_status":
    case "cmd_list_branches":
      return [] as never;
    default:
      return undefined as never;
  }
};

function makeStore() {
  const graphCached = new Set<string>();
  const store = createRepoStore({
    invoke,
    storage: memoryStorage(),
    caseInsensitive: true,
    graph: {
      showRepo: (path) => void (path && graphCached.add(path)),
      loadGraph: async () => {},
      evict: (path) => void graphCached.delete(path),
    },
    filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
    terminals: { countFor: () => 0, hostedFor: () => 0 },
  });
  return { store, graphCached };
}

type Store = ReturnType<typeof makeStore>["store"];

/** Opens `count` repositories and exercises each so every per-tab record fills. */
async function openAndExercise(store: Store, prefix: string, count: number, group?: string): Promise<string[]> {
  const paths: string[] = [];
  for (let i = 0; i < count; i += 1) {
    const path = `/retention/${prefix}${i}`;
    paths.push(path);
    expect(await store.openRepo(path)).toBe(true);
    if (group) store.setTabGroup(get(store).openTabs.find((tab) => tab.path === path)!.id, group);
    expect((await store.stageFile("a.txt")).ok).toBe(true); // arms the echo window, files a journal action
    for (const tab of ["work", "code", "history", "insights", "work"] as const) store.setActiveTab(tab);
    await store.refresh(path);
  }
  return paths;
}

const ids = (store: Store) => get(store).openTabs.map((tab) => tab.id);

describe("closing repositories leaves nothing behind", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  const routes: Array<[string, (store: Store) => Promise<void>]> = [
    ["closeTab", async (store) => { for (const id of ids(store)) await store.closeTab(id); }],
    ["closeOtherTabs + closeTab", async (store) => { const [keep] = ids(store); await store.closeOtherTabs(keep); await store.closeTab(keep); }],
    ["closeTabsToTheRight + closeTab", async (store) => { const [keep] = ids(store); await store.closeTabsToTheRight(keep); await store.closeTab(keep); }],
    ["closeGroup", async (store) => { await store.closeGroup("batch"); }],
  ];

  for (const [name, close] of routes) {
    it(`returns to baseline through ${name}`, async () => {
      const { store, graphCached } = makeStore();
      const records = store.retainedSessionRecords();
      const journals = harnessStore.cachedRepositories();
      const timers = vi.getTimerCount();

      await openAndExercise(store, name.replace(/\W/g, ""), 10, "batch");
      expect(store.retainedSessionRecords(), "the exercise must fill the records").toBeGreaterThan(records);
      expect(harnessStore.cachedRepositories()).toBe(journals + 10);
      expect(graphCached.size).toBeGreaterThan(0);

      await close(store);

      expect(get(store).openTabs).toHaveLength(0);
      expect(store.retainedSessionRecords()).toBe(records);
      expect(harnessStore.cachedRepositories()).toBe(journals);
      expect(graphCached.size).toBe(0);
      await vi.runOnlyPendingTimersAsync();
      expect(vi.getTimerCount()).toBe(timers);
    });
  }

  it("holds steady across many open/close cycles", async () => {
    const { store } = makeStore();
    const baseline = { records: store.retainedSessionRecords(), journals: harnessStore.cachedRepositories() };
    for (let cycle = 0; cycle < 5; cycle += 1) {
      await openAndExercise(store, `cycle${cycle}-`, 8);
      for (const id of ids(store)) await store.closeTab(id);
      expect({ records: store.retainedSessionRecords(), journals: harnessStore.cachedRepositories() }).toEqual(baseline);
    }
    await vi.runOnlyPendingTimersAsync();
    expect(vi.getTimerCount()).toBe(0);
  });
});
