import { describe, expect, it } from "vitest";
import { writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import { memoryStorage } from "../../repos/persist";
import type { FilterState } from "../filterStore";
import { gitActionCount } from "../../desktop/exitGuard";

/**
 * Its own file on purpose. A subscriber that throws leaves Svelte's
 * module-level subscriber queue non-empty (svelte/src/store/shared/index.js),
 * so every store update after it in the same module graph stops being
 * delivered. Isolated here, that damage cannot silently blind a later test.
 */

const invoke: InvokeFn = async (cmd, args) => {
  switch (cmd) {
    case "cmd_resolve_repo": {
      const path = String(args?.repoPath ?? "");
      return { path, name: path.split("/").pop(), is_bare: false } as never;
    }
    case "cmd_list_branches":
    case "cmd_get_status":
      return [] as never;
    case "cmd_list_tags":
      return { tags: [], truncated: false } as never;
    case "cmd_stash_list":
      return { entries: [], truncated: false } as never;
    case "cmd_watch_repo":
      return String(args?.repoPath) as never;
    case "cmd_branch_stats":
      return { compared_to: "", updates: [], computed: 0, cached: 0, capped: false } as never;
    case "cmd_repo_operation":
      return null as never;
    case "cmd_commit":
    case "cmd_push":
    case "cmd_unwatch_repo":
    case "cmd_note_tab_activated":
    case "cmd_set_recent_menu":
      return undefined as never;
    default:
      throw new Error(`unexpected command ${cmd}`);
  }
};

function makeStore() {
  const filter = writable<FilterState>({ searchQuery: "", selectedBranch: null });
  return createRepoStore({
    invoke,
    storage: memoryStorage(),
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: {
      subscribe: filter.subscribe,
      setSearch: (query: string) => filter.update((s) => ({ ...s, searchQuery: query })),
      selectBranch: (branch: string | null) => filter.update((s) => ({ ...s, selectedBranch: branch })),
      clear: () => filter.set({ searchQuery: "", selectedBranch: null }),
    },
  });
}

describe("repoStore mutation queue with a failing subscriber", () => {
  it("a throwing activity subscriber does not strand the path's queue", async () => {
    const store = makeStore();
    await store.openRepo("/r/a");
    let boom = false;
    const stop = store.mutationActivity.subscribe((activity) => {
      if (boom && gitActionCount(activity) === 0) throw new Error("subscriber failed");
    });
    boom = true;
    await store.commit("m").catch(() => undefined);
    boom = false;
    const next = await Promise.race([
      store.push().then(() => "ran"),
      new Promise<string>((resolve) => setTimeout(() => resolve("stuck"), 500)),
    ]);
    stop();
    expect(next).toBe("ran");
  });
});
