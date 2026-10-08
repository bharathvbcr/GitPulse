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
import { computeTabLayout, stackKeyFor } from "../../repos/tabGroups";

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

describe("a family learned after the first open", () => {
  /** A store restored from storage that already holds MAIN and one worktree. */
  async function restoredWith(invoke: InvokeFn) {
    const storage = memoryStorage();
    const seed = createRepoStore({
      invoke: makeInvoke(),
      storage,
      caseInsensitive: true,
      graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
      filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
    });
    await seed.openRepo(MAIN);
    await seed.openRepo(wt("a"));
    const store = createRepoStore({
      invoke,
      storage,
      caseInsensitive: true,
      graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
      filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
    });
    await store.restoreWorkspace();
    return store;
  }
  const familyOf = (store: ReturnType<typeof makeStore>, path: string) =>
    get(store).openTabs.find((t) => t.path === path)?.family ?? null;

  it("stacks a repository restored untrusted once trust is granted", async () => {
    // Restore defers trust: every checkout came back with no family. Granting
    // trust on activation hydrated the tab but never said which repository it
    // was, so the whole family stayed split until the app restarted.
    let trusted = false;
    const base = makeInvoke();
    const store = await restoredWith(
      makeInvoke({
        cmd_resolve_repo: async (cmd, args) => {
          if (!trusted) throw new Error("REPOSITORY_TRUST_REQUIRED: not trusted");
          return base(cmd, args);
        },
        cmd_repository_trust: async (_cmd, args) => {
          trusted = true;
          return { path: String(args?.repoPath), scope: "repository", worktrees: 2, identity: "x" } as never;
        },
      }),
    );
    expect(familyOf(store, MAIN)).toBeNull();
    const id = (path: string) => get(store).openTabs.find((t) => t.path === path)!.id;
    await store.activateTab(id(MAIN));
    await store.activateTab(id(wt("a")));
    expect(familyOf(store, MAIN)).toBeTruthy();
    expect(familyOf(store, wt("a"))).toBe(familyOf(store, MAIN));
  });

  it("learns the family on refresh when the checkout could not be resolved at restore", async () => {
    let reachable = false;
    const base = makeInvoke();
    const store = await restoredWith(
      makeInvoke({
        cmd_resolve_repo: async (cmd, args) => {
          if (!reachable) throw new Error("Cannot access path: volume not mounted");
          return base(cmd, args);
        },
      }),
    );
    expect(familyOf(store, wt("a"))).toBeNull();
    reachable = true;
    const id = (path: string) => get(store).openTabs.find((t) => t.path === path)!.id;
    await store.activateTab(id(MAIN));
    await store.refresh();
    await store.activateTab(id(wt("a")));
    expect(familyOf(store, wt("a"))).toBeTruthy();
    expect(familyOf(store, wt("a"))).toBe(familyOf(store, MAIN));
    // With the family known, grouping by folder files the worktree under its
    // repository's folder instead of a folder literally named "worktrees".
    store.groupByParentFolder();
    expect(get(store).openTabs.find((t) => t.path === wt("a"))?.group).toBe("devtools");
  });

  it("restores the whole strip, families included, before the first git read — and reads the active tab first", async () => {
    const storage = memoryStorage();
    const deps = (invoke: InvokeFn) => ({
      invoke,
      storage,
      caseInsensitive: true,
      graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
      filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
    });
    const seed = createRepoStore(deps(makeInvoke()));
    // The worktree is saved BEFORE its repository, and the active tab is last.
    await seed.openRepo(wt("a"));
    await seed.openRepo(OTHER);
    await seed.openRepo(MAIN);
    const idOf = (path: string) => get(seed).openTabs.find((t) => t.path === path)!.id;
    expect(seed.arrangeTabs([idOf(wt("a")), idOf(OTHER), idOf(MAIN)])).toBe(true);
    await seed.activateTab(idOf(MAIN));

    let atFirstRead: { paths: string[]; families: (string | null)[] } | null = null;
    const reads: string[] = [];
    const base = makeInvoke();
    let store: ReturnType<typeof createRepoStore>;
    store = createRepoStore(
      deps(
        makeInvoke({
          cmd_get_status: async (cmd, args) => {
            reads.push(String(args?.repoPath));
            if (!atFirstRead) {
              const tabs = get(store).openTabs;
              atFirstRead = { paths: tabs.map((t) => t.path), families: tabs.map((t) => t.family ?? null) };
            }
            return base(cmd, args);
          },
        }),
      ),
    );
    await store.restoreWorkspace();
    expect(atFirstRead).not.toBeNull();
    expect(atFirstRead!.paths).toEqual([wt("a"), OTHER, MAIN]);
    expect(atFirstRead!.families[0]).toBeTruthy();
    expect(atFirstRead!.families[0]).toBe(atFirstRead!.families[2]);
    expect(reads[0]).toBe(MAIN);
    expect(new Set(reads)).toEqual(new Set([MAIN, wt("a"), OTHER]));
    expect(get(store).openTabs.find((t) => t.isActive)?.path).toBe(MAIN);
  });

  it("asks once per hydrate and never loops when the backend reports no family at all", async () => {
    let resolves = 0;
    const store = makeStore(
      makeInvoke({
        cmd_resolve_repo: async (_cmd, args) => {
          resolves += 1;
          const path = String(args?.repoPath);
          return { path, name: "x", is_bare: false } as never;
        },
      }),
    );
    await store.openRepo(MAIN);
    const after = resolves;
    await store.refresh();
    await store.refresh();
    // An answer of "no common directory" is an answer; only a failed resolve
    // leaves the family worth asking about again.
    expect(resolves).toBe(after);
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

  it("joins its repository's group, so it stacks instead of opening as another repository", async () => {
    // Before: MAIN sat in "core", the new worktree opened ungrouped at the far
    // end, and the stack key (group, family) could never match — the worktree
    // was drawn as a separate repository.
    const store = makeStore();
    await store.openRepo(MAIN);
    store.setTabGroup(get(store).openTabs[0].id, "core");
    await store.openRepo(OTHER);
    await store.openRepo(wt("a"));
    const groupOf = (path: string) => get(store).openTabs.find((t) => t.path === path)?.group ?? null;
    expect(groupOf(wt("a"))).toBe("core");
    expect(ids(store)).toEqual([MAIN, wt("a"), OTHER]);
    const familyKey = get(store).openTabs[0].family!;
    const layout = computeTabLayout(get(store).openTabs, [], undefined, [], { enabled: true });
    expect(layout.stacks.map((stack) => stack.id)).toEqual([`stack:${stackKeyFor("core", familyKey)}`]);
  });

  it("follows the checkout of its repository the reader used last when the family spans groups", async () => {
    const store = makeStore();
    await store.openRepo(MAIN);
    await store.openRepo(wt("a"));
    const id = (path: string) => get(store).openTabs.find((t) => t.path === path)!.id;
    store.setTabGroup(id(MAIN), "core");
    store.setTabGroup(id(wt("a")), "agents");
    await store.activateTab(id(wt("a")));
    await store.openRepo(wt("b"));
    expect(get(store).openTabs.find((t) => t.path === wt("b"))?.group).toBe("agents");
  });

  it("unfolds the group it joins when it opens on screen, and leaves it folded when it opens out of sight", async () => {
    const store = makeStore();
    await store.openRepo(MAIN);
    await store.openRepo(OTHER);
    const id = (path: string) => get(store).openTabs.find((t) => t.path === path)!.id;
    store.setTabGroup(id(MAIN), "core");
    store.setGroupCollapsed("core", true);
    await store.openRepo(wt("quiet"), { activate: false });
    expect(get(store).openTabs.find((t) => t.path === wt("quiet"))?.group).toBe("core");
    expect(get(store).collapsedGroups).toContain("core");
    await store.openRepo(wt("shown"));
    expect(get(store).collapsedGroups).not.toContain("core");
  });

  it("keeps an explicit group, and leaves a repository without a family where it opened", async () => {
    const store = makeStore();
    await store.openRepo(MAIN);
    store.setTabGroup(get(store).openTabs[0].id, "core");
    await store.openRepo(wt("a"), { group: "elsewhere" });
    expect(get(store).openTabs.find((t) => t.path === wt("a"))?.group).toBe("elsewhere");
    await store.openRepo(OTHER);
    expect(get(store).openTabs.find((t) => t.path === OTHER)?.group ?? null).toBeNull();
  });

  it("does not move or regroup a checkout that was already open", async () => {
    const store = makeStore();
    await store.openRepo(MAIN);
    await store.openRepo(OTHER);
    await store.openRepo(wt("a"));
    const id = (path: string) => get(store).openTabs.find((t) => t.path === path)!.id;
    store.setTabGroup(id(MAIN), "core");
    await store.openRepo(wt("a"));
    expect(get(store).openTabs.find((t) => t.path === wt("a"))?.group ?? null).toBeNull();
  });
});

describe("placement under random interleavings", () => {
  /** Small deterministic PRNG so a failure names a seed that reproduces it. */
  function prng(seed: number) {
    let s = seed >>> 0;
    return () => {
      s = (s + 0x6d2b79f5) >>> 0;
      let t = s;
      t = Math.imul(t ^ (t >>> 15), t | 1);
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }
  const ROOTS = ["/code/a/one", "/code/a/two", "/code/b/three"];
  const POOL = [
    ...ROOTS,
    ...ROOTS.flatMap((root) => ["x", "y", "z"].map((slug) => `${root}/.claude/worktrees/${slug}`)),
    "/code/b/lonely",
  ];
  const GROUPS = [null, "core", "side"];
  const familyRoot = (path: string) => {
    const at = path.indexOf("/.claude/worktrees/");
    return at >= 0 ? path.slice(0, at) : path;
  };

  for (const stacking of [true, false]) {
    it(`keeps every invariant across 300 seeded runs (stacking ${stacking ? "on" : "off"})`, async () => {
      interfaceStore.setStackWorktreeTabs(stacking);
      for (let seed = 1; seed <= 300; seed += 1) {
        resetStackState();
        const rand = prng(seed);
        const pick = <T,>(items: readonly T[]) => items[Math.floor(rand() * items.length)];
        const store = makeStore();
        const tabs = () => get(store).openTabs;
        for (let step = 0; step < 24; step += 1) {
          const roll = rand();
          const before = tabs();
          if (roll < 0.55) {
            const path = pick(POOL);
            const activate = rand() < 0.6;
            const wasOpen = before.find((t) => t.path === path);
            const members = before.filter((t) => familyRoot(t.path) === familyRoot(path) && t.path !== path);
            await store.openRepo(path, { activate });
            const after = tabs();
            const where = after.findIndex((t) => t.path === path);
            const ctx = `seed ${seed} step ${step} open ${path}`;
            expect(where, ctx).toBeGreaterThanOrEqual(0);
            expect(new Set(after.map((t) => t.id)).size, ctx).toBe(after.length);
            if (wasOpen) {
              // Reopening never moves or regroups what the reader arranged.
              expect(after.map((t) => t.id), ctx).toEqual(before.map((t) => t.id));
              expect(after[where].group ?? null, ctx).toBe(wasOpen.group ?? null);
            } else if (members.length > 0) {
              const group = after[where].group ?? null;
              expect(members.map((m) => m.group ?? null), ctx).toContain(group);
              const prev = after[where - 1];
              expect(prev && familyRoot(prev.path) === familyRoot(path), ctx).toBe(true);
              expect(prev?.group ?? null, ctx).toBe(group);
            } else {
              expect(where, ctx).toBe(after.length - 1);
              expect(after[where].group ?? null, ctx).toBeNull();
            }
          } else if (roll < 0.75 && before.length > 0) {
            store.setTabGroup(pick(before).id, pick(GROUPS));
          } else if (roll < 0.9 && before.length > 0) {
            await store.activateTab(pick(before).id);
          } else if (before.length > 0) {
            await store.closeTab(pick(before).id);
          }
        }
      }
    }, 60_000);
  }
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

describe("familyOf: a checkout's repository without opening it", () => {
  it("answers from an open tab without asking the backend, and resolves an unopened checkout without opening it", async () => {
    const resolves: string[] = [];
    const store = makeStore(makeInvoke({ cmd_resolve_repo: async (_cmd, args) => { const path = String(args?.repoPath); resolves.push(path); return { path, name: "x", is_bare: false, common_dir: commonDirFor(path) } as never; } }));
    await store.openRepo(MAIN);
    const open = get(store).openTabs[0].family;
    resolves.length = 0;
    expect(await store.familyOf(MAIN)).toBe(open);
    expect(resolves).toEqual([]);
    expect(await store.familyOf(wt("never-opened"))).toBe(open);
    expect(resolves).toEqual([wt("never-opened")]);
    expect(ids(store)).toEqual([MAIN]);
  });

  it("is null, never a guess, when the checkout cannot be resolved or reports no common directory", async () => {
    const failing = makeStore(makeInvoke({ cmd_resolve_repo: async () => { throw new Error("REPOSITORY_TRUST_REQUIRED"); } }));
    expect(await failing.familyOf(wt("a"))).toBeNull();
    const silent = makeStore(makeInvoke({ cmd_resolve_repo: async (_cmd, args) => ({ path: String(args?.repoPath), name: "x", is_bare: false }) as never }));
    expect(await silent.familyOf(wt("a"))).toBeNull();
    expect(await silent.familyOf("")).toBeNull();
  });
});
