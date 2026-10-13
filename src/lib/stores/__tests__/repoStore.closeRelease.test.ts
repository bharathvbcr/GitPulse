/**
 * Three routes close tabs (closeTab, closeOtherTabs, closeTabsToTheRight).
 * Each must end in the one release routine: the per-tab records the hydrate,
 * poll and stats paths keep, and the harness store's per-repository journal
 * cache, are dropped with the tab whichever route closed it.
 */
import { describe, expect, it } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import { harnessStore } from "../harnessStore";
import { memoryStorage } from "../../repos/persist";

const invoke: InvokeFn = async (cmd, args) => {
  const path = String(args?.repoPath ?? "");
  switch (cmd) {
    case "cmd_resolve_repo": return { path, name: path.split("/").pop(), is_bare: false, common_dir: `${path}/.git` } as never;
    case "cmd_watch_repo": return path as never;
    case "cmd_list_tags": return { tags: [], truncated: false } as never;
    case "cmd_stash_list": return { entries: [], truncated: false } as never;
    case "cmd_branch_stats": return { compared_to: null, updates: [], computed: 0, cached: 0, capped: false } as never;
    case "cmd_get_status": case "cmd_list_branches": return [] as never;
    default: return undefined as never;
  }
};

function makeStore() {
  return createRepoStore({
    invoke,
    storage: memoryStorage(),
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
    terminals: { countFor: () => 0, hostedFor: () => 0 },
  });
}

/** Opens `/closerelease/<n>` tabs and gives each a journal bucket, as an action recorded against it would. */
async function open(count: number) {
  const store = makeStore();
  const paths = Array.from({ length: count }, (_, i) => `/closerelease/${count}-${i}`);
  for (const path of paths) {
    expect(await store.openRepo(path)).toBe(true);
    harnessStore.recordAction({ repoPath: path, kind: "probe", label: "probe", ok: true });
  }
  return { store, paths, ids: get(store).openTabs.map((tab) => tab.id) };
}

describe("closing tabs releases what they held", () => {
  it("holds records for open tabs (so the checks below can fail)", async () => {
    const { store, paths } = await open(3);
    expect(store.retainedSessionRecords()).toBeGreaterThan(0);
    expect(harnessStore.cachedRepositories()).toBeGreaterThanOrEqual(paths.length);
    for (const id of get(store).openTabs.map((tab) => tab.id)) await store.closeTab(id);
  });

  it("closeTab", async () => {
    const { store, paths } = await open(2);
    const before = harnessStore.cachedRepositories();
    await store.closeTab(get(store).openTabs[0].id);
    expect(harnessStore.cachedRepositories()).toBe(before - 1);
    await store.closeTab(get(store).openTabs[0].id);
    expect(store.retainedSessionRecords()).toBe(0);
    expect(harnessStore.cachedRepositories()).toBe(before - 2);
    expect(paths).toHaveLength(2);
  });

  it("closeOtherTabs", async () => {
    const { store, ids } = await open(4);
    const before = harnessStore.cachedRepositories();
    await store.closeOtherTabs(ids[1]);
    expect(get(store).openTabs).toHaveLength(1);
    expect(harnessStore.cachedRepositories()).toBe(before - 3);
    await store.closeTab(ids[1]);
    expect(store.retainedSessionRecords()).toBe(0);
  });

  it("closeTabsToTheRight", async () => {
    const { store, ids } = await open(4);
    const before = harnessStore.cachedRepositories();
    await store.closeTabsToTheRight(ids[1]);
    expect(get(store).openTabs).toHaveLength(2);
    expect(harnessStore.cachedRepositories()).toBe(before - 2);
    await store.closeOtherTabs(ids[0]);
    await store.closeTab(ids[0]);
    expect(store.retainedSessionRecords()).toBe(0);
  });

  it("keeps the journal of a repository another tab still shows", async () => {
    const store = makeStore();
    expect(await store.openRepo("/closerelease/shared")).toBe(true);
    harnessStore.recordAction({ repoPath: "/closerelease/shared", kind: "probe", label: "probe", ok: true });
    const before = harnessStore.cachedRepositories();
    await store.closeTab(get(store).openTabs[0].id);
    expect(harnessStore.cachedRepositories()).toBe(before - 1);
  });
});
