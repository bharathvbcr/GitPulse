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
  const state = { failing: false, statusCalls: 0, failedCalls: 0 };
  const statuses = [{ path: "README.md", status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 }];
  const invoke: InvokeFn = async (cmd, args) => {
    const path = String(args?.repoPath ?? "");
    switch (cmd) {
      case "cmd_resolve_repo":
        return { path, name: "r", is_bare: false, common_dir: `${path}/.git` } as never;
      case "cmd_get_status":
        state.statusCalls += 1;
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
});
