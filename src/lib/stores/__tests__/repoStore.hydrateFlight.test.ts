/**
 * A repository's snapshot is read one at a time, a tab that was just read is
 * not read again on activation, the status poll rests in proportion to what it
 * costs, and branch churn is not asked for when no tip moved.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, ACTIVATION_FRESH_MS, type BranchInfo, type InvokeFn } from "../repoStore";
import { memoryStorage } from "../../repos/persist";

type Counters = {
  /** Commands called, by name. */
  calls: Record<string, number>;
  /** `cmd_get_status` calls in flight right now, and the most there ever were. */
  statusInFlight: number;
  statusPeak: number;
  /** Start times of every `cmd_get_status` call. */
  statusStarts: number[];
};

function branch(name: string, tip: string, extra: Partial<BranchInfo> = {}): BranchInfo {
  return {
    name,
    is_current: name === "main",
    is_remote: false,
    tip_commit_id: tip,
    ahead_count: 0,
    behind_count: 0,
    is_default: name === "main",
    is_gone: false,
    last_commit_timestamp: 0,
    last_author: "ada",
    last_summary: "init",
    ...extra,
  } as BranchInfo;
}

function harness(options: { statusMs?: number; tips?: () => Record<string, string> } = {}) {
  const counters: Counters = { calls: {}, statusInFlight: 0, statusPeak: 0, statusStarts: [] };
  const statuses = [{ path: "README.md", status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 }];
  const tips = options.tips ?? (() => ({ main: "aaa", topic: "bbb" }));
  const invoke: InvokeFn = async (cmd, args) => {
    counters.calls[cmd] = (counters.calls[cmd] ?? 0) + 1;
    const path = String(args?.repoPath ?? "");
    switch (cmd) {
      case "cmd_resolve_repo":
        return { path, name: path.split("/").pop(), is_bare: false, common_dir: `${path}/.git` } as never;
      case "cmd_get_status": {
        counters.statusStarts.push(Date.now());
        counters.statusInFlight += 1;
        counters.statusPeak = Math.max(counters.statusPeak, counters.statusInFlight);
        if (options.statusMs) await new Promise((resolve) => setTimeout(resolve, options.statusMs));
        counters.statusInFlight -= 1;
        return statuses as never;
      }
      case "cmd_list_branches":
        return Object.entries(tips()).map(([name, tip]) => branch(name, tip)) as never;
      case "cmd_list_tags": return { tags: [], truncated: false } as never;
      case "cmd_stash_list": return { entries: [], truncated: false } as never;
      case "cmd_repo_operation": return null as never;
      case "cmd_last_fetch_at": return null as never;
      case "cmd_watch_repo": return path as never;
      case "cmd_branch_stats":
        return {
          compared_to: "main",
          updates: Object.entries(tips())
            .filter(([name]) => name !== "main")
            .map(([name, tip]) => ({
              name,
              tip_commit_id: tip,
              is_remote: false,
              remote_name: null,
              additions: 1,
              deletions: 0,
              files_changed: 1,
              commits_ahead_of_base: 1,
              commits_behind_base: 0,
            })),
          computed: 1,
          cached: 0,
          capped: false,
          compute_failures: 0,
        } as never;
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
  return { store, counters };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("hydrate is single-flight per repository", () => {
  it("under a 4 s status and a watcher event every 250 ms for 20 s, reads never pile up", async () => {
    vi.useFakeTimers();
    const { store, counters } = harness({ statusMs: 4_000 });
    const opening = store.openRepo("/r/storm");
    await vi.advanceTimersByTimeAsync(4_100);
    await opening;
    counters.statusPeak = 0;
    const before = counters.calls.cmd_get_status ?? 0;

    for (let elapsed = 0; elapsed < 20_000; elapsed += 250) {
      void store.handleRepoChanged("/r/storm");
      await vi.advanceTimersByTimeAsync(250);
    }
    await vi.advanceTimersByTimeAsync(9_000);

    // A running read plus the status poll, never a read per event.
    expect(counters.statusPeak).toBeLessThanOrEqual(2);
    // 20 s of storm against 4 s reads: a handful of snapshots, not one per
    // event (80 events) and not one per debounce window.
    expect((counters.calls.cmd_get_status ?? 0) - before).toBeLessThanOrEqual(10);
  });

  it("a refresh asked while another runs shares one trailing read, however many ask", async () => {
    vi.useFakeTimers();
    const { store, counters } = harness({ statusMs: 4_000 });
    const opening = store.openRepo("/r/shared");
    await vi.advanceTimersByTimeAsync(4_100);
    await opening;
    const before = counters.calls.cmd_list_branches ?? 0;

    const asks = Array.from({ length: 30 }, () => store.refresh());
    await vi.advanceTimersByTimeAsync(10);
    expect((counters.calls.cmd_list_branches ?? 0) - before, "thirty asks, one read started").toBe(1);
    await vi.advanceTimersByTimeAsync(4_100);
    expect((counters.calls.cmd_list_branches ?? 0) - before, "the one owed behind it has started").toBe(2);
    await vi.advanceTimersByTimeAsync(4_100);
    await Promise.all(asks);
    expect((counters.calls.cmd_list_branches ?? 0) - before, "and nothing further").toBe(2);
  });
});

describe("activating a tab that was just read", () => {
  async function twoTabs() {
    vi.useFakeTimers();
    const h = harness();
    await h.store.openRepo("/r/a");
    await h.store.openRepo("/r/b");
    const [a] = get(h.store).openTabs.filter((tab) => tab.path === "/r/a");
    return { ...h, aId: a.id };
  }

  it("issues no snapshot read within the freshness window", async () => {
    const { store, counters, aId } = await twoTabs();
    await vi.advanceTimersByTimeAsync(1_000);
    const reads = () =>
      ["cmd_get_status", "cmd_list_branches", "cmd_list_tags", "cmd_repo_operation", "cmd_stash_list", "cmd_last_fetch_at"].map(
        (cmd) => counters.calls[cmd] ?? 0,
      );
    const before = reads();

    await store.activateTab(aId);

    expect(get(store).currentPath).toBe("/r/a");
    expect(reads()).toEqual(before);
  });

  it("reads again once the snapshot is older than the window", async () => {
    const { store, counters, aId } = await twoTabs();
    await vi.advanceTimersByTimeAsync(ACTIVATION_FRESH_MS + 1_000);
    const before = counters.calls.cmd_list_branches ?? 0;

    await store.activateTab(aId);

    expect(counters.calls.cmd_list_branches ?? 0).toBe(before + 1);
  });

  it("reads again when a watcher event for it is still owed", async () => {
    const { store, counters, aId } = await twoTabs();
    await vi.advanceTimersByTimeAsync(1_000);
    await store.handleRepoChanged("/r/a");
    const before = counters.calls.cmd_list_branches ?? 0;

    await store.activateTab(aId);

    expect(counters.calls.cmd_list_branches ?? 0).toBe(before + 1);
  });
});

describe("the status poll rests in proportion to what it costs", () => {
  it("with a 4 s status and a live watcher, git status runs at most 10% of the time", async () => {
    vi.useFakeTimers();
    const { store, counters } = harness({ statusMs: 4_000 });
    const opening = store.openRepo("/r/slow");
    await vi.advanceTimersByTimeAsync(4_100);
    await opening;
    const startedAt = Date.now();
    counters.statusStarts.length = 0;

    await vi.advanceTimersByTimeAsync(10 * 60_000);

    const busyMs = counters.statusStarts.length * 4_000;
    expect(busyMs / (Date.now() - startedAt)).toBeLessThanOrEqual(0.1);
    expect(counters.statusStarts.length).toBeGreaterThan(0);
  });

  it("an idle repository with a live watcher is polled at most once a minute", async () => {
    vi.useFakeTimers();
    const { store, counters } = harness();
    await store.openRepo("/r/idle");
    // A minute of nothing is what makes it quiet.
    await vi.advanceTimersByTimeAsync(2 * 60_000);
    counters.statusStarts.length = 0;
    const from = Date.now();

    await vi.advanceTimersByTimeAsync(10 * 60_000);

    expect(counters.statusStarts.length).toBeLessThanOrEqual(10);
    for (let i = 1; i < counters.statusStarts.length; i += 1) {
      expect(counters.statusStarts[i] - counters.statusStarts[i - 1]).toBeGreaterThanOrEqual(59_000);
    }
    expect(Date.now() - from).toBe(10 * 60_000);
  });
});

describe("branch churn is asked for only when a tip moved", () => {
  it("makes no cmd_branch_stats call for a refresh that left every tip alone", async () => {
    const { store, counters } = harness();
    await store.openRepo("/r/churn");
    await vi.waitFor(() => expect(counters.calls.cmd_branch_stats ?? 0).toBe(1));
    await vi.waitFor(() => expect(get(store).branches.find((b) => b.name === "topic")?.compared_to).toBe("main"));

    await store.refresh();
    await store.refresh();
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(counters.calls.cmd_branch_stats).toBe(1);
  });

  it("asks again when a tip moves, and when the default branch moves", async () => {
    let tips: Record<string, string> = { main: "aaa", topic: "bbb" };
    const { store, counters } = harness({ tips: () => tips });
    await store.openRepo("/r/moved");
    await vi.waitFor(() => expect(counters.calls.cmd_branch_stats ?? 0).toBe(1));
    await vi.waitFor(() => expect(get(store).branches.find((b) => b.name === "topic")?.compared_to).toBe("main"));

    tips = { main: "aaa", topic: "ccc" };
    await store.refresh();
    await vi.waitFor(() => expect(counters.calls.cmd_branch_stats).toBe(2));

    // Only the default moved: topic's own tip is the same, but what it is
    // measured against is not.
    await vi.waitFor(() => expect(get(store).branches.find((b) => b.name === "topic")?.tip_commit_id).toBe("ccc"));
    tips = { main: "ddd", topic: "ccc" };
    await store.refresh();
    await vi.waitFor(() => expect(counters.calls.cmd_branch_stats).toBe(3));
  });
});

describe("closing a tab leaves nothing of the new records behind", () => {
  it("drops the flight, the freshness stamp and the drain signature", async () => {
    const { store } = harness();
    await store.openRepo("/r/gone");
    await vi.waitFor(() => expect(store.retainedSessionRecords()).toBeGreaterThan(0));
    await store.closeTab(get(store).openTabs[0].id);
    expect(store.retainedSessionRecords()).toBe(0);
  });
});
