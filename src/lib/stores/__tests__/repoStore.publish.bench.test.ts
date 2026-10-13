/**
 * What one repoStore publish costs when many tabs are open.
 *
 * ft-0eb00c5c asked whether a publish, which walks every tab and serialises
 * two key lists to compare them with last time, is worth replacing with a
 * per-tab revision diff, and said to measure first and close the card if the
 * cost was negligible. This is that measurement, kept so the answer is
 * checkable and so a regression is caught.
 *
 * Measured on the code as it stood (Apple silicon, min of 15 samples of 400
 * publishes, CPU time): 1 tab 6.2-6.6 us, 50 tabs 12.0-12.8 us per publish.
 * The 50-tab publish therefore costs about 6 us more than a one-tab publish;
 * the status poll publishes at most every few seconds per active tab and a
 * hydrate a handful of times, so the steady cost of the walk and the two
 * `JSON.stringify` calls is well under 0.01% of one core. The row cache in
 * `projection.ts` is what keeps it there: an unchanged tab costs one
 * reference comparison. The card was closed without changing publish.
 *
 * The bound below is what keeps that true. It fails for a publish that
 * re-projects every tab, or that serialises something per tab, and is loose
 * enough (an order of magnitude over the measured figure) for a loaded host.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
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

async function storeWith(count: number) {
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
    terminals: { countFor: () => 0, hostedFor: () => 0 },
  });
  await store.restoreWorkspace();
  expect(get(store).openTabs).toHaveLength(count);
  return store;
}

/**
 * One publish per call and nothing else: the active tab's error alternates
 * between set and cleared, which publishes every time and, unlike a setting
 * that persists, does not also write the workspace.
 */
function publishes(store: Awaited<ReturnType<typeof storeWith>>, times: number): void {
  for (let i = 0; i < times; i += 1) store.setError(i % 2 === 0 ? "bench" : null);
}

function cpuMs(work: () => void): number {
  const before = process.cpuUsage();
  work();
  const used = process.cpuUsage(before);
  return (used.user + used.system) / 1000;
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("repoStore publish", () => {
  it("stays cheap at 50 tabs, and the numbers are on record", async () => {
    const SIZES = [1, 50, 500];
    const PER_SAMPLE = 400;
    const stores = await Promise.all(SIZES.map((count) => storeWith(count)));
    for (const store of stores) publishes(store, PER_SAMPLE);
    const best = SIZES.map(() => Infinity);
    for (let round = 0; round < 15; round += 1) {
      stores.forEach((store, index) => {
        best[index] = Math.min(best[index], cpuMs(() => publishes(store, PER_SAMPLE)));
      });
    }
    const perPublishUs = best.map((ms) => (ms * 1000) / PER_SAMPLE);
    // Written to stdout directly: the runner hides the console of a passing test.
    process.stdout.write(
      `repoStore publish, min of 15 x ${PER_SAMPLE}: ${SIZES.map((count, i) => `${count} tabs ${perPublishUs[i].toFixed(1)} us`).join(", ")}\n`,
    );
    // Measured ~12 us at 50 tabs. 100 us is 8x that: slack for a loaded host,
    // and well under what a per-tab serialisation or re-projection costs.
    expect(perPublishUs[1]).toBeLessThan(100);
  }, STRESS_TIMEOUT_MS);
});
