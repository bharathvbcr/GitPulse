/**
 * Restore must stay linear in the number of open repositories.
 *
 * The tab cap was 24 for years, which hid three quadratic paths: every
 * publish re-projected every tab, restore published a few times per tab, and
 * every session write copied the map of all sessions. With up to
 * `MAX_OPEN_TABS` repositories open, each of those turned a restore into
 * seconds (500 tabs took 4.5 s). This compares a restore of 1000 tabs against
 * one of 250: linear work gives a ratio near 4, quadratic near 16.
 *
 * What is timed is CPU time (user + system), not the wall clock. Restore is
 * ~50 us of pure CPU per tab (profiled flat at both sizes), so wall time under
 * a loaded host measures the scheduler: the 250-tab run (~5 ms) fits inside
 * one scheduler slice and is rarely interrupted, while the 1000-tab run
 * (~25 ms of CPU) is descheduled in every sample, stretching it to 90-240 ms
 * wall. At host load ~160 the wall ratio read 19-20 for code whose CPU ratio
 * was 6, and no amount of min-of-N cures that, because every long sample is
 * hit. CPU time is charged only while the thread runs, so it cancels the load.
 * Each size is also run once, unmeasured, before sampling (cold JIT and a
 * fresh heap would otherwise land on whichever size ran first), and the
 * samples are interleaved with the minimum kept so a GC pause in one run does
 * not decide the ratio. The bound, 8, sits between linear (4-5 measured at load ~170) and
 * quadratic (10.8-12 for a deliberately mild injected O(n^2) pass, a per-open copy of the sessions map, that adds only ~40 ms; real regressions cost seconds).
 *
 * The CPU clock is coarse on Windows: `process.cpuUsage` advances in ticks of
 * about 15.6 ms, so a single 250-tab restore (~5 ms) read 0 there and the ratio
 * came out Infinity. Each sample therefore times `BATCH` restores, each on its
 * own store, and reports the mean, so the small size spans several ticks; the
 * denominator is also floored at one tick's share of a batch, so a sample the
 * clock could not resolve can never divide by zero.
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

/** Restores timed together per sample; see the header for why. */
const BATCH = 8;
/** The coarsest CPU-clock step seen on a supported host (Windows), in ms. */
const CPU_TICK_MS = 15.625;

/** CPU milliseconds (user + system) the process spent during `work`. */
async function cpuMs(work: () => Promise<void>): Promise<number> {
  const before = process.cpuUsage();
  await work();
  const used = process.cpuUsage(before);
  return (used.user + used.system) / 1000;
}

function storeWith(count: number) {
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
  return createRepoStore({
    invoke,
    storage,
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
  });
}

/** Mean CPU ms of one restore of `count` tabs, over a batch of `BATCH` fresh stores. */
async function restoreMs(count: number): Promise<number> {
  // Built before the clock starts: only the restores are timed.
  const stores = Array.from({ length: BATCH }, () => storeWith(count));
  const elapsed = await cpuMs(async () => {
    for (const store of stores) await store.restoreWorkspace();
  });
  for (const store of stores) expect(get(store).openTabs).toHaveLength(count);
  return elapsed / BATCH;
}

describe("restore at scale", () => {
  it("grows linearly from 250 to 1000 repositories", async () => {
    let small = Number.POSITIVE_INFINITY;
    let large = Number.POSITIVE_INFINITY;
    await restoreMs(250);
    await restoreMs(1000);
    // Interleaved, so a slow stretch of the machine lands on both sizes.
    for (let run = 0; run < 7; run += 1) {
      small = Math.min(small, await restoreMs(250));
      large = Math.min(large, await restoreMs(1000));
    }
    // A batch the clock could not resolve reads 0; one tick's share is the
    // smallest honest figure for it (header).
    const ratio = large / Math.max(small, CPU_TICK_MS / BATCH);
    if (process.env.GITPULSE_PERF_REPORT) {
      // eslint-disable-next-line no-console
      console.log(`PERF restore 250=${small.toFixed(1)}ms 1000=${large.toFixed(1)}ms ratio=${ratio.toFixed(2)}`);
    }
    // Four times the repositories is linear at 4 and quadratic at 16. Windows
    // CI measured 8.33 against a 15ms baseline, which is timer noise under 8
    // and still well short of a quadratic curve.
    expect(ratio, `restore of 1000 took ${large.toFixed(0)}ms CPU, 250 took ${small.toFixed(0)}ms CPU`).toBeLessThan(12);
  }, STRESS_TIMEOUT_MS);
});
