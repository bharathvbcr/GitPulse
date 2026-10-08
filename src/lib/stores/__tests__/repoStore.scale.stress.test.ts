/**
 * Restore must stay linear in the number of open repositories.
 *
 * The tab cap was 24 for years, which hid three quadratic paths: every
 * publish re-projected every tab, restore published a few times per tab, and
 * every session write copied the map of all sessions. With up to
 * `MAX_OPEN_TABS` repositories open, each of those turned a restore into
 * seconds (500 tabs took 4.5 s). This compares a restore of 1000 tabs against
 * one of 250: linear work gives a ratio near 4, quadratic near 16. A ratio
 * cancels the machine out, so a loaded runner cannot fail it the way an
 * absolute budget would, and the fastest of five runs drops the samples a
 * concurrent build disturbed. The bound, 10, sits between the two: the
 * quadratic code measured 13-16 on an idle machine, the linear code 5, and
 * 14 once under a fully loaded suite with the short run inside one scheduler
 * slice, which is why the bound is not tighter.
 */
import { describe, expect, it } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import { memoryStorage, savePersistedWorkspace, WORKSPACE_VERSION } from "../../repos/persist";
import { STRESS_TIMEOUT_MS } from "../../__tests__/perfBudget";

const invoke: InvokeFn = async (cmd, args) => {
  const path = String(args?.repoPath ?? "");
  switch (cmd) {
    case "cmd_resolve_repo":
      return { path, name: path.split("/").pop(), is_bare: false, common_dir: `${path}/.git` } as never;
    case "cmd_list_branches":
    case "cmd_get_status":
      return [] as never;
    case "cmd_list_tags":
      return { tags: [], truncated: false } as never;
    case "cmd_stash_list":
      return { entries: [], truncated: false } as never;
    case "cmd_branch_stats":
      return { compared_to: null, updates: [], computed: 0, cached: 0, capped: false } as never;
    case "cmd_repo_operation":
      return null as never;
    default:
      return undefined as never;
  }
};

async function restoreMs(count: number): Promise<number> {
  const storage = memoryStorage();
  savePersistedWorkspace(storage, {
    version: WORKSPACE_VERSION,
    tabs: Array.from({ length: count }, (_, i) => ({
      path: `/code/r${i}`, pinned: false, viewTab: "work" as const, terminalOpen: false, searchQuery: "", selectedBranch: null,
    })),
    activePath: "/code/r0",
    recents: [],
    lastClosed: [],
  });
  const store = createRepoStore({
    invoke,
    storage,
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
  });
  const started = performance.now();
  await store.restoreWorkspace();
  const elapsed = performance.now() - started;
  expect(get(store).openTabs).toHaveLength(count);
  return elapsed;
}

describe("restore at scale", () => {
  it("grows linearly from 250 to 1000 repositories", async () => {
    let small = Number.POSITIVE_INFINITY;
    let large = Number.POSITIVE_INFINITY;
    // Interleaved, so a slow stretch of the machine lands on both sizes.
    for (let run = 0; run < 5; run += 1) {
      small = Math.min(small, await restoreMs(250));
      large = Math.min(large, await restoreMs(1000));
    }
    const ratio = large / small;
    if (process.env.GITPULSE_PERF_REPORT) {
      // eslint-disable-next-line no-console
      console.log(`PERF restore 250=${small.toFixed(1)}ms 1000=${large.toFixed(1)}ms ratio=${ratio.toFixed(2)}`);
    }
    expect(ratio, `restore of 1000 took ${large.toFixed(0)}ms, 250 took ${small.toFixed(0)}ms`).toBeLessThan(10);
  }, STRESS_TIMEOUT_MS);
});
