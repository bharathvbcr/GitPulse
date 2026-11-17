/**
 * Two asynchronous reads a tab keeps in flight must not leave the tab
 * looking finished when they were not:
 *
 *  - a multi-batch branch-stats drain abandoned because the tab was activated
 *    (generation bump) must end with stats for the live generation, or with
 *    `statsPending` still true;
 *  - a snapshot read that started before a status poll must not apply its
 *    older statuses over the poll's newer ones.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type BranchInfo, type InvokeFn } from "../repoStore";
import { memoryStorage } from "../../repos/persist";

function branch(name: string, current: boolean): BranchInfo {
  return {
    name,
    is_current: current,
    is_remote: false,
    tip_commit_id: `tip-${name}`,
    ahead_count: 0,
    behind_count: 0,
    is_default: current,
    is_gone: false,
    last_commit_timestamp: 0,
    last_author: "ada",
    last_summary: "init",
  } as BranchInfo;
}

const NAMES = ["main", "one", "two", "three"];

function fileStatus(path: string) {
  return { path, status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 };
}

interface Gate {
  release: () => void;
}

function harness() {
  const state = {
    /** `cmd_branch_stats` answers computed so far, per repository. */
    computed: new Map<string, number>(),
    statsCalls: [] as string[],
    /** The next `cmd_branch_stats` waits here until released. */
    holdNextStats: false,
    heldStats: null as Gate | null,
    /** Statuses a `cmd_get_status` answers with. */
    statusAnswer: [fileStatus("old.txt")],
    holdNextStatus: false,
    heldStatus: null as Gate | null,
  };
  const invoke: InvokeFn = async (cmd, args) => {
    const path = String(args?.repoPath ?? "");
    switch (cmd) {
      case "cmd_resolve_repo":
        return { path, name: path.split("/").pop(), is_bare: false, common_dir: `${path}/.git` } as never;
      case "cmd_get_status": {
        const answer = state.statusAnswer;
        if (state.holdNextStatus) {
          state.holdNextStatus = false;
          await new Promise<void>((resolve) => {
            state.heldStatus = { release: resolve };
          });
        }
        return answer as never;
      }
      case "cmd_list_branches":
        return NAMES.map((name, i) => branch(name, i === 0)) as never;
      case "cmd_list_tags": return { tags: [], truncated: false } as never;
      case "cmd_stash_list": return { entries: [], truncated: false } as never;
      case "cmd_repo_operation": return null as never;
      case "cmd_last_fetch_at": return null as never;
      case "cmd_watch_repo": return path as never;
      case "cmd_branch_stats": {
        state.statsCalls.push(path);
        if (state.holdNextStats) {
          state.holdNextStats = false;
          await new Promise<void>((resolve) => {
            state.heldStats = { release: resolve };
          });
        }
        // The backend computes one branch per call and answers with every
        // branch computed so far, until none is left.
        const done = Math.min((state.computed.get(path) ?? 0) + 1, NAMES.length);
        state.computed.set(path, done);
        return {
          compared_to: "main",
          updates: NAMES.slice(0, done).map((name) => ({
            name,
            tip_commit_id: `tip-${name}`,
            is_remote: false,
            remote_name: null,
            additions: 1,
            deletions: 1,
            files_changed: 1,
            commits_ahead_of_base: 1,
            commits_behind_base: 0,
          })),
          computed: 1,
          cached: done - 1,
          capped: done < NAMES.length,
          compute_failures: 0,
        } as never;
      }
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

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("a branch-stats drain abandoned by tab activation", () => {
  it("ends with stats for the live generation, or with statsPending still true", async () => {
    const { store, state } = harness();
    state.holdNextStats = true;
    expect(await store.openRepo("/r/drained")).toBe(true);
    const tabId = get(store).openTabs[0].id;
    await vi.waitFor(() => expect(state.heldStats).not.toBeNull());

    const generationBefore = get(store).generation;

    // Switch away and back while the first batch is still being computed.
    expect(await store.openRepo("/r/other")).toBe(true);
    await store.activateTab(tabId);
    expect(get(store).currentPath).toBe("/r/drained");
    expect(generationBefore).not.toBe(get(store).generation);

    state.heldStats!.release();

    // Whatever the tab claims at rest must be true: stats for every branch,
    // or an honest "still computing".
    await vi.waitFor(() => {
      const view = get(store);
      const missing = view.branches.filter((b) => b.compared_to === undefined);
      expect(missing.length === 0 || view.statsPending).toBe(true);
    });
    await vi.waitFor(() => {
      const view = get(store);
      expect(view.branches.filter((b) => b.compared_to === undefined)).toHaveLength(0);
      expect(view.statsPending).toBe(false);
    });
  });
});

describe("a slow snapshot read racing a status poll", () => {
  it("does not apply its older statuses over the poll's newer ones", async () => {
    vi.useFakeTimers();
    const { store, state } = harness();
    await store.openRepo("/r/raced");
    expect(get(store).statuses.map((s) => s.path)).toEqual(["old.txt"]);

    // A watcher-driven refresh starts reading the status and stalls.
    state.holdNextStatus = true;
    const refreshed = store.refresh("/r/raced", "status");
    await vi.advanceTimersByTimeAsync(0);
    expect(state.heldStatus).not.toBeNull();

    // The poll runs while it is stalled and sees the newer answer.
    state.statusAnswer = [fileStatus("new.txt")];
    for (let i = 0; i < 40 && get(store).statuses[0]?.path !== "new.txt"; i += 1) {
      await vi.advanceTimersByTimeAsync(1_000);
    }
    expect(get(store).statuses.map((s) => s.path)).toEqual(["new.txt"]);

    // The stalled read finally returns what it read before the poll.
    state.heldStatus!.release();
    await refreshed;
    await vi.advanceTimersByTimeAsync(0);
    expect(get(store).statuses.map((s) => s.path)).toEqual(["new.txt"]);
  });
});
