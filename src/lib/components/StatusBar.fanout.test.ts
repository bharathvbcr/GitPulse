import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { derived, get, type Readable } from "svelte/store";
import { describe, expect, it } from "vitest";
import { memoryStorage } from "../repos/persist";
import { createRepoStore, type InvokeFn } from "../stores/repoStore";

/**
 * Card ft-7d89e82b (AC8, StatusBar): the bar reads only the narrow fields it
 * renders, each through a field store of `repoStore`, so a publish that leaves
 * those fields alone wakes nothing. vitest resolves Svelte's server build, where
 * `$derived` is plain, so the bar itself cannot be counted here. What is checked
 * instead is each link of the chain: which stores the component's source binds,
 * and, on the real store, how often each of them notifies per publish.
 */
const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "StatusBar.svelte"), "utf8");

/** `const xStore = repoStore.x;` bindings: the field stores the component subscribes to. */
function boundFieldStores(): Record<string, string> {
  const bound: Record<string, string> = {};
  for (const match of source.matchAll(/const\s+(\w+)\s*=\s*repoStore\.(\w+)\s*;/g)) bound[match[2]] = match[1];
  return bound;
}

const status = (path: string, staged = false) => ({
  path, status_code: "M", is_staged: staged, is_conflicted: false, additions: 1, deletions: 0,
});

async function openedStore() {
  let statuses = [status("a.ts")];
  const invoke: InvokeFn = async (cmd, args) => {
    switch (cmd) {
      case "cmd_resolve_repo": return { path: String(args?.repoPath), name: "r", is_bare: false } as never;
      case "cmd_list_branches": return [] as never;
      case "cmd_get_status": return statuses as never;
      case "cmd_list_tags": return { tags: [], truncated: false } as never;
      case "cmd_stash_list": return { entries: [], truncated: false } as never;
      case "cmd_repo_operation": return null as never;
      case "cmd_watch_repo": return String(args?.repoPath) as never;
      case "cmd_unwatch_repo": case "cmd_note_tab_activated": case "cmd_set_recent_menu": return undefined as never;
      default: throw new Error(`unexpected command ${cmd}`);
    }
  };
  const store = createRepoStore({
    invoke,
    storage: memoryStorage(),
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
  });
  await store.openRepo("/r/statusbar");
  return { store, setStatuses: (next: ReturnType<typeof status>[]) => { statuses = next; } };
}

describe("StatusBar subscribes through field stores", () => {
  it("reads no whole-store value and binds the fields it renders", () => {
    expect(source).not.toMatch(/\$repoStore\b/);
    const names = Object.keys(boundFieldStores()).sort();
    expect(names).toEqual(
      ["branches", "conflictedCount", "currentBranch", "isLoading", "operation", "stagedCount", "statuses", "statusUnavailableSince", "watch"].sort(),
    );
  });

  it("a publish that changes none of those fields notifies none of them, and re-runs no derivation", async () => {
    const { store, setStatuses } = await openedStore();
    const fields = boundFieldStores() as Record<string, string>;
    const stores = store as unknown as Record<string, Readable<unknown>>;
    const notified: Record<string, number> = {};
    const stops = Object.keys(fields).map((name) => {
      expect(stores[name], `repoStore.${name} must exist`).toBeDefined();
      let first = true;
      notified[name] = 0;
      return stores[name].subscribe(() => {
        if (first) first = false;
        else notified[name] += 1;
      });
    });
    // The bar's two derivations of its own, counted on the real field stores.
    let branchInfoRuns = 0;
    let dirtyRuns = 0;
    const stopBranch = derived([stores.branches, stores.currentBranch], ([branches, branch]) => {
      branchInfoRuns += 1;
      return (branches as { name: string }[]).find((b) => b.name === branch);
    }).subscribe(() => {});
    const stopDirty = derived(stores.statuses, (statuses) => {
      dirtyRuns += 1;
      return (statuses as unknown[]).length;
    }).subscribe(() => {});
    branchInfoRuns = 0;
    dirtyRuns = 0;

    // Publishes the bar does not render: file selection, a commit selection, an error.
    store.selectFilePath("a.ts");
    store.selectFilePath("b.ts");
    store.setError("boom");
    store.setError(null);
    expect(notified).toEqual(Object.fromEntries(Object.keys(fields).map((name) => [name, 0])));
    expect(branchInfoRuns).toBe(0);
    expect(dirtyRuns).toBe(0);

    // A change to statuses wakes the statuses chain only.
    setStatuses([status("a.ts"), status("b.ts")]);
    await store.refresh("/r/statusbar", "status");
    expect(notified.statuses).toBe(1);
    expect(dirtyRuns).toBe(1);
    expect(branchInfoRuns).toBe(0);
    expect(get(stores.statuses as Readable<unknown[]>)).toHaveLength(2);

    stopBranch();
    stopDirty();
    for (const stop of stops) stop();
  });
});
