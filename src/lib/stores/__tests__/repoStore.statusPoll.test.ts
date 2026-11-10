/**
 * A status poll that keeps failing must say so. It used to swallow every
 * rejection, so a repository whose `git status` had stopped answering showed
 * its last file list as if it were current.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type BranchInfo, type InvokeFn } from "../repoStore";
import { memoryStorage } from "../../repos/persist";
import { diagnostics } from "../../diagnostics/diagnostics";

function branch(name: string): BranchInfo {
  return {
    name,
    is_current: true,
    is_remote: false,
    tip_commit_id: "aaa",
    ahead_count: 0,
    behind_count: 0,
    is_default: true,
    is_gone: false,
    last_commit_timestamp: 0,
    last_author: "ada",
    last_summary: "init",
  } as BranchInfo;
}

function harness() {
  const state = {
    failing: false,
    statusCalls: 0,
    /** The arguments of every `cmd_get_status`, in order. */
    statusArgs: [] as Array<Record<string, unknown> | undefined>,
    failedCalls: 0,
    /** Every answer names a different file, so every poll announces a change. */
    changing: false,
    /** The next status read waits here until `held.reject()` is called. */
    holdNext: false,
    held: null as { reject: (error: Error) => void } | null,
  };
  const statuses = [{ path: "README.md", status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 }];
  const invoke: InvokeFn = async (cmd, args) => {
    const path = String(args?.repoPath ?? "");
    switch (cmd) {
      case "cmd_resolve_repo":
        return { path, name: "r", is_bare: false, common_dir: `${path}/.git` } as never;
      case "cmd_get_status":
        state.statusCalls += 1;
        state.statusArgs.push(args);
        if (state.holdNext) {
          state.holdNext = false;
          return new Promise((_resolve, reject) => {
            state.held = { reject };
          }) as never;
        }
        if (state.changing) {
          return [{ ...statuses[0], path: `file-${state.statusCalls}.txt` }] as never;
        }
        if (state.failing) {
          state.failedCalls += 1;
          throw new Error("git status exploded");
        }
        return statuses as never;
      case "cmd_list_branches": return [branch("main")] as never;
      case "cmd_list_tags": return { tags: [], truncated: false } as never;
      case "cmd_stash_list": return { entries: [], truncated: false } as never;
      case "cmd_repo_operation": return null as never;
      case "cmd_last_fetch_at": return null as never;
      case "cmd_watch_repo": return path as never;
      case "cmd_branch_stats":
        return { compared_to: "main", updates: [], computed: 0, cached: 0, capped: false, compute_failures: 0 } as never;
      default: return undefined as never;
    }
  };
  const store = createRepoStore({
    invoke,
    storage: memoryStorage(),
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
    terminals: { countFor: () => 0, hostedFor: () => 0 },
  });
  return { store, state };
}

/** Advances time until `n` more failing polls have run. */
async function untilFailures(state: { failedCalls: number }, n: number) {
  const target = state.failedCalls + n;
  for (let i = 0; i < 40 && state.failedCalls < target; i += 1) await vi.advanceTimersByTimeAsync(1_000);
  expect(state.failedCalls).toBe(target);
}

/** The warnings about the status poll; other subsystems warn through the same sink. */
function statusWarnings(warn: { mock: { calls: unknown[][] } }) {
  return warn.mock.calls.filter(([, detail]) => String(detail).includes("git status"));
}

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("the status poll's read", () => {
  it("alone asks the backend to answer from a kept read; a hydrate always reads", async () => {
    vi.useFakeTimers();
    const { store, state } = harness();
    await store.openRepo("/r/polled");
    const hydrated = state.statusArgs.length;
    expect(hydrated).toBeGreaterThan(0);
    for (let i = 0; i < 40 && state.statusArgs.length === hydrated; i += 1) {
      await vi.advanceTimersByTimeAsync(1_000);
    }
    expect(state.statusArgs.length).toBeGreaterThan(hydrated);
    for (const args of state.statusArgs.slice(0, hydrated)) {
      expect(args?.skipIfUnchanged).toBeUndefined();
    }
    expect(state.statusArgs[hydrated]).toEqual({ repoPath: "/r/polled", skipIfUnchanged: true });
  });
});

describe("a status poll that keeps failing", () => {
  it("marks the session unavailable once on the third consecutive failure, and clears on success", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(diagnostics, "warn");
    const { store, state } = harness();
    await store.openRepo("/r/failing");
    expect(get(store).statusUnavailableSince).toBeNull();

    state.failing = true;
    await untilFailures(state, 2);
    expect(get(store).statusUnavailableSince).toBeNull();
    expect(statusWarnings(warn)).toHaveLength(0);

    await untilFailures(state, 1);
    const since = get(store).statusUnavailableSince;
    expect(since).toEqual(expect.any(Number));
    expect(statusWarnings(warn)).toHaveLength(1);
    // It is a notice on the bar, not the blocking session error.
    expect(get(store).error).toBeNull();

    await untilFailures(state, 2);
    expect(get(store).statusUnavailableSince).toBe(since);
    expect(statusWarnings(warn)).toHaveLength(1);

    // The same statuses come back: a quiet-repo poll that publishes nothing
    // must still clear the notice.
    state.failing = false;
    const before = state.statusCalls;
    for (let i = 0; i < 40 && state.statusCalls === before; i += 1) await vi.advanceTimersByTimeAsync(1_000);
    await vi.advanceTimersByTimeAsync(0);
    expect(get(store).statusUnavailableSince).toBeNull();
  });

  it("does not mark it when a success separates the failures", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(diagnostics, "warn");
    const { store, state } = harness();
    await store.openRepo("/r/flaky");

    for (let round = 0; round < 3; round += 1) {
      state.failing = true;
      await untilFailures(state, 2);
      state.failing = false;
      const before = state.statusCalls;
      for (let i = 0; i < 40 && state.statusCalls === before; i += 1) await vi.advanceTimersByTimeAsync(1_000);
      await vi.advanceTimersByTimeAsync(0);
    }

    expect(get(store).statusUnavailableSince).toBeNull();
    expect(statusWarnings(warn)).toHaveLength(0);
  });

  it("does not count a poll that git answered but a listener choked on", async () => {
    vi.useFakeTimers();
    const warn = vi.spyOn(diagnostics, "warn");
    const { store, state } = harness();
    await store.openRepo("/r/listener");
    store.onPolledChange(() => {
      throw new Error("subscriber exploded");
    });

    // One answered poll whose listener throws...
    state.changing = true;
    const before = state.statusCalls;
    for (let i = 0; i < 40 && state.statusCalls === before; i += 1) await vi.advanceTimersByTimeAsync(1_000);
    await vi.advanceTimersByTimeAsync(0);
    state.changing = false;
    expect(statusWarnings(warn)).toHaveLength(0);

    // ...must not stand toward the threshold: two real failures are one short.
    state.failing = true;
    await untilFailures(state, 2);
    expect(get(store).statusUnavailableSince).toBeNull();
    expect(statusWarnings(warn)).toHaveLength(0);
  });

  it("does not count a failure of a poll that a newer snapshot superseded", async () => {
    vi.useFakeTimers();
    const { store, state } = harness();
    await store.openRepo("/r/superseded");

    // A poll goes out and stalls; a full refresh lands while it waits.
    state.holdNext = true;
    for (let i = 0; i < 40 && !state.held; i += 1) await vi.advanceTimersByTimeAsync(1_000);
    const held = state.held;
    expect(held).not.toBeNull();
    await store.refresh();
    held!.reject(new Error("late failure"));
    await vi.advanceTimersByTimeAsync(0);

    // The stale failure must not stand toward the threshold: two more are
    // one short of it.
    state.failing = true;
    await untilFailures(state, 2);
    expect(get(store).statusUnavailableSince).toBeNull();
  });
});
