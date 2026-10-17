/**
 * `handleRepoChanged` is the one place watcher events enter the store, and it
 * ends the join generation of identical in-flight reads before anything is
 * asked for: a refresh requested because the repository changed must not ride
 * a read that began before the change (ipc/invoke.ts).
 */
import { describe, expect, it, vi } from "vitest";
import { writable } from "svelte/store";

const invalidate = vi.hoisted(() => vi.fn());
vi.mock("../../ipc/invoke", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../ipc/invoke")>()),
  invalidateSharedReads: invalidate,
}));

import { createRepoStore, type InvokeFn } from "../repoStore";
import { memoryStorage } from "../../repos/persist";

const invoke: InvokeFn = async (cmd, args) => {
  const path = String(args?.repoPath ?? "");
  switch (cmd) {
    case "cmd_resolve_repo": return { path, name: path.split("/").pop(), is_bare: false, common_dir: `${path}/.git` } as never;
    case "cmd_list_tags": return { tags: [], truncated: false } as never;
    case "cmd_stash_list": return { entries: [], truncated: false } as never;
    case "cmd_get_status": case "cmd_list_branches": return [] as never;
    case "cmd_watch_repo": return path as never;
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

describe("watcher events end the join generation", () => {
  it("for an open repository, a path nobody has open, and an event with no path", async () => {
    const store = makeStore();
    await store.openRepo("/r/open");
    invalidate.mockClear();

    await store.handleRepoChanged("/r/open");
    expect(invalidate).toHaveBeenCalledTimes(1);
    await store.handleRepoChanged("/r/not-open");
    expect(invalidate).toHaveBeenCalledTimes(2);
    await store.handleRepoChanged(null);
    expect(invalidate).toHaveBeenCalledTimes(3);
  });
});
