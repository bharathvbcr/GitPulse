/**
 * The store's half of worktree stacking: the family each tab carries, where a
 * new checkout lands, and the shortcuts that step through the strip as drawn.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import type { FilterState } from "../filterStore";
import { interfaceStore } from "../interfaceStore";
import { memoryStorage } from "../../repos/persist";
import { resetStackState, setStackExpanded } from "../../repos/stackState";
import { stackKeyFor } from "../../repos/tabGroups";

const MAIN = "/code/devtools/GitPulse";
const OTHER = "/code/web/site";
const wt = (slug: string) => `${MAIN}/.claude/worktrees/${slug}`;

/** The backend's answer: every checkout of MAIN shares MAIN/.git. */
function commonDirFor(path: string): string {
  const at = path.indexOf("/.claude/worktrees/");
  return `${at >= 0 ? path.slice(0, at) : path}/.git`;
}

function makeInvoke(overrides: Partial<Record<string, InvokeFn>> = {}): InvokeFn {
  return async (cmd, args) => {
    const override = overrides[cmd];
    if (override) return override(cmd, args);
    const path = String(args?.repoPath ?? "");
    switch (cmd) {
      case "cmd_resolve_repo":
        return { path, name: path.split("/").pop(), is_bare: false, common_dir: commonDirFor(path) } as never;
      case "cmd_list_branches":
      case "cmd_get_status":
        return [] as never;
      case "cmd_list_tags":
        return { tags: [], truncated: false } as never;
      case "cmd_repo_operation":
        return null as never;
      case "cmd_stash_list":
        return { entries: [], truncated: false } as never;
      case "cmd_branch_stats":
        return { compared_to: null, updates: [], computed: 0, cached: 0, capped: false } as never;
      case "cmd_watch_repo":
        return path as never;
      case "cmd_unwatch_repo":
      case "cmd_set_recent_menu":
        return undefined as never;
      default:
        throw new Error(`unexpected command ${cmd}`);
    }
  };
}

function makeStore(invoke: InvokeFn = makeInvoke()) {
  const filter = writable<FilterState>({ searchQuery: "", selectedBranch: null });
  return createRepoStore({
    invoke,
    storage: memoryStorage(),
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: {
      subscribe: filter.subscribe,
      setSearch: () => {},
      selectBranch: () => {},
      clear: () => {},
    },
  });
}

const ids = (store: ReturnType<typeof makeStore>) => get(store).openTabs.map((tab) => tab.path);
const active = (store: ReturnType<typeof makeStore>) => get(store).openTabs.find((tab) => tab.isActive)?.path;

beforeEach(() => {
  resetStackState();
  interfaceStore.setStackWorktreeTabs(true);
});

afterEach(() => {
  resetStackState();
  interfaceStore.setStackWorktreeTabs(true);
});

describe("repository family on open tabs", () => {
  it("gives every checkout of a repository the same family and names its root", async () => {
    const store = makeStore();
    await store.openRepo(MAIN);
    await store.openRepo(wt("a"));
    await store.openRepo(OTHER);
    const byPath = new Map(get(store).openTabs.map((tab) => [tab.path, tab]));
    expect(byPath.get(wt("a"))?.family).toBe(byPath.get(MAIN)?.family);
    expect(byPath.get(MAIN)?.familyRoot).toBe(MAIN);
    expect(byPath.get(OTHER)?.family).not.toBe(byPath.get(MAIN)?.family);
  });

  it("leaves the family unknown when the backend does not report a common directory", async () => {
    // Older backends and every harness fixture send {path, name, is_bare}.
    const store = makeStore(
      makeInvoke({
        cmd_resolve_repo: async (_cmd, args) => {
          const path = String(args?.repoPath);
          return { path, name: "x", is_bare: false } as never;
        },
      }),
    );
    await store.openRepo(MAIN);
    expect(get(store).openTabs[0].family).toBeNull();
    expect(get(store).openTabs[0].familyRoot).toBeNull();
  });

  it("keeps the last good family when a later resolve fails, and drops it when one says unreadable", async () => {
    let mode: "ok" | "fail" | "unreadable" = "ok";
    const base = makeInvoke();
    const store = makeStore(
      makeInvoke({
        cmd_resolve_repo: async (cmd, args) => {
          if (mode === "fail") throw new Error("Cannot access path");
          const resolved = (await base(cmd, args)) as unknown as Record<string, unknown>;
          return (mode === "unreadable" ? { ...resolved, common_dir: null } : resolved) as never;
        },
      }),
    );
    await store.openRepo(MAIN);
    const family = get(store).openTabs[0].family;
    expect(family).toBeTruthy();
    mode = "fail";
    await store.openRepo(MAIN, { allowBroken: true });
    expect(get(store).openTabs[0].family).toBe(family);
    mode = "unreadable";
    await store.openRepo(MAIN);
    expect(get(store).openTabs[0].family).toBeNull();
  });
});

describe("where a new checkout lands", () => {
  it("opens a worktree beside its repository instead of at the far end of the strip", async () => {
    const store = makeStore();
    await store.openRepo(MAIN);
    await store.openRepo(OTHER);
    await store.openRepo("/code/web/docs");
    await store.openRepo(wt("a"));
    await store.openRepo(wt("b"));
    expect(ids(store)).toEqual([MAIN, wt("a"), wt("b"), OTHER, "/code/web/docs"]);
  });

  it("does not reorder a restore: the saved order is the user's", async () => {
    const storage = memoryStorage();
    const first = createRepoStore({
      invoke: makeInvoke(),
      storage,
      caseInsensitive: true,
      graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
      filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
    });
    await first.openRepo(MAIN);
    await first.openRepo(OTHER);
    await first.openRepo(wt("a"));
    // Put the worktree at the end on purpose, then restore into a fresh store.
    const idOf = (path: string) => get(first).openTabs.find((t) => t.path === path)!.id;
    expect(first.arrangeTabs([idOf(MAIN), idOf(OTHER), idOf(wt("a"))])).toBe(true);
    const second = createRepoStore({
      invoke: makeInvoke(),
      storage,
      caseInsensitive: true,
      graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
      filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
    });
    await second.restoreWorkspace();
    expect(get(second).openTabs.map((t) => t.path)).toEqual([MAIN, OTHER, wt("a")]);
  });

  it("only clusters within the same group", async () => {
    const store = makeStore();
    await store.openRepo(MAIN);
    store.setTabGroup(get(store).openTabs[0].id, "core");
    await store.openRepo(OTHER);
    await store.openRepo(wt("a"));
    // MAIN is in "core" and the new worktree is ungrouped: no family neighbour.
    expect(ids(store)).toEqual([MAIN, OTHER, wt("a")]);
  });
});

describe("shortcuts step through the strip as drawn", () => {
  async function opened() {
    const store = makeStore();
    await store.openRepo(MAIN);
    await store.openRepo(wt("a"));
    await store.openRepo(wt("b"));
    await store.openRepo(OTHER);
    await store.activateTab(get(store).openTabs[0].id);
    return store;
  }

  it("treats a folded stack as one stop for next/previous", async () => {
    const store = await opened();
    expect(active(store)).toBe(MAIN);
    await store.nextTab();
    expect(active(store)).toBe(OTHER);
    await Promise.resolve();
    await store.nextTab();
    expect(active(store)).toBe(MAIN);
  });

  it("returns to the checkout last used when cycling back into a folded stack", async () => {
    const store = await opened();
    await store.activateTab(get(store).openTabs.find((t) => t.path === wt("b"))!.id);
    await store.nextTab();
    expect(active(store)).toBe(OTHER);
    await Promise.resolve();
    await store.prevTab();
    expect(active(store)).toBe(wt("b"));
  });

  it("visits every checkout once the stack is unfolded", async () => {
    const store = await opened();
    const family = get(store).openTabs[0].family!;
    setStackExpanded(stackKeyFor(null, family), true);
    const visited: (string | undefined)[] = [];
    for (let i = 0; i < 4; i += 1) {
      await store.nextTab();
      await Promise.resolve();
      visited.push(active(store));
    }
    expect(visited).toEqual([wt("a"), wt("b"), OTHER, MAIN]);
  });

  it("maps a number key to the nth thing on the strip", async () => {
    const store = await opened();
    await store.activateTabAt(1);
    expect(active(store)).toBe(OTHER);
    await store.activateTabAt(0);
    expect(active(store)).toBe(MAIN);
    await store.activateTabAt(5);
    expect(active(store)).toBe(MAIN);
    await store.activateTabAt(-1);
    await store.activateTabAt(1.5);
    expect(active(store)).toBe(MAIN);
  });

  it("falls back to every tab in stored order with stacking turned off", async () => {
    const store = await opened();
    interfaceStore.setStackWorktreeTabs(false);
    await store.nextTab();
    expect(active(store)).toBe(wt("a"));
    await store.activateTabAt(3);
    expect(active(store)).toBe(OTHER);
  });
});

describe("moves from the palette and keyboard", () => {
  it("moves a folded stack as one unit, never swapping with a checkout hidden inside it", async () => {
    const store = makeStore();
    for (const p of [MAIN, wt("a"), OTHER]) await store.openRepo(p);
    const id = (path: string) => get(store).openTabs.find((t) => t.path === path)!.id;
    // Drawn: {GitPulse: MAIN, a}, OTHER. Moving the hidden checkout right
    // moves the whole stack past OTHER, not MAIN past its own worktree.
    expect(store.canMoveTab(id(wt("a")), 1)).toBe(true);
    expect(store.moveTabBy(id(wt("a")), 1)).toBe(true);
    expect(ids(store)).toEqual([OTHER, MAIN, wt("a")]);
    expect(store.canMoveTab(id(MAIN), 1)).toBe(false);
    expect(store.moveTabBy(id(MAIN), 1)).toBe(false);
    expect(store.moveTabToEdge(id(OTHER), "end")).toBe(true);
    expect(ids(store)).toEqual([MAIN, wt("a"), OTHER]);
  });

  it("refuses unknown ids and non-integer steps without touching the order", async () => {
    const store = makeStore();
    for (const p of [MAIN, OTHER]) await store.openRepo(p);
    const before = ids(store);
    for (const delta of [0, 0.5, Number.NaN]) expect(store.moveTabBy(get(store).openTabs[0].id, delta)).toBe(false);
    expect(store.moveTabBy("/missing", 1)).toBe(false);
    expect(store.canMoveTab("/missing", 1)).toBe(false);
    expect(ids(store)).toEqual(before);
  });
});

describe("arrangeTabs and grouping", () => {
  it("applies a full order, and refuses a stale one without half-applying a regroup", async () => {
    const store = makeStore();
    await store.openRepo(MAIN);
    await store.openRepo(OTHER);
    expect(store.arrangeTabs([OTHER, MAIN].map((p) => p.toLowerCase()))).toBe(true);
    expect(ids(store)).toEqual([OTHER, MAIN]);
    expect(store.arrangeTabs(["/gone", MAIN.toLowerCase()], { ids: [MAIN.toLowerCase()], group: "x" })).toBe(false);
    expect(get(store).openTabs.every((t) => !t.group)).toBe(true);
  });

  it("regroups and opens the destination group so a dropped tab never vanishes", async () => {
    const store = makeStore();
    await store.openRepo(MAIN);
    await store.openRepo(OTHER);
    const [main, other] = get(store).openTabs.map((t) => t.id);
    store.setTabGroup(main, "core");
    store.setGroupCollapsed("core", true);
    expect(store.arrangeTabs([main, other], { ids: [other], group: "core" })).toBe(true);
    expect(get(store).openTabs.find((t) => t.id === other)?.group).toBe("core");
    expect(get(store).collapsedGroups).not.toContain("core");
  });

  it("groups a worktree by where its repository lives", async () => {
    const store = makeStore();
    await store.openRepo(MAIN);
    await store.openRepo(OTHER);
    await store.openRepo(wt("a"));
    store.groupByParentFolder();
    const groupOf = (path: string) => get(store).openTabs.find((t) => t.path === path)?.group;
    expect(groupOf(wt("a"))).toBe("devtools");
    expect(groupOf(MAIN)).toBe("devtools");
    expect(groupOf(OTHER)).toBe("web");
  });
});
