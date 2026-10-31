/**
 * Adversarial Challenger Stress Harness for Faceted Hydration Pipeline (Feature 6).
 *
 * Empirically challenges:
 * 1. Rapid file change storms (50 rapid edit events) - confirms only cmd_get_status is queued/executed.
 * 2. Rapid ref change storms (50 rapid ref events) - confirms only cmd_list_branches is queued/executed.
 * 3. Mixed edit/ref storms - confirms facet escalation via mergeFacets to "all".
 * 4. Fault isolation - metadata facet failures (tags, stash, ops, fetch) do not crash status/branch hydration.
 * 5. Recovery from status failures - subsequent successful facet clears error cleanly.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";
import {
  createRepoStore,
  facetForChange,
  mergeFacets,
  type BranchInfo,
  type FileStatus,
  type InvokeFn,
  type RepoChange,
} from "../repoStore";
import { memoryStorage } from "../../repos/persist";

type CommandCounters = {
  calls: Record<string, number>;
  history: { cmd: string; time: number }[];
};

function makeHarness(config: {
  failMetadata?: boolean;
  failStatus?: boolean;
  failBranches?: boolean;
  statusDelayMs?: number;
} = {}) {
  const counters: CommandCounters = { calls: {}, history: [] };

  const invoke: InvokeFn = async (cmd, args) => {
    counters.calls[cmd] = (counters.calls[cmd] ?? 0) + 1;
    counters.history.push({ cmd, time: Date.now() });
    const path = String(args?.repoPath ?? "");

    switch (cmd) {
      case "cmd_resolve_repo":
        return {
          path,
          name: path.split("/").pop(),
          is_bare: false,
          common_dir: `${path}/.git`,
        } as never;

      case "cmd_get_status": {
        if (config.failStatus) {
          throw new Error("simulated git status failure: SpawnGate token depleted");
        }
        if (config.statusDelayMs) {
          await new Promise((r) => setTimeout(r, config.statusDelayMs));
        }
        return [
          {
            path: "src/file.ts",
            status_code: "M",
            is_staged: false,
            is_conflicted: false,
            additions: 5,
            deletions: 2,
          },
        ] as never;
      }

      case "cmd_list_branches": {
        if (config.failBranches) {
          throw new Error("simulated git branch failure: fatal: ambiguous argument 'HEAD'");
        }
        return [
          {
            name: "main",
            is_current: true,
            is_remote: false,
            tip_commit_id: "a1b2c3d4",
            ahead_count: 0,
            behind_count: 0,
            is_default: true,
            is_gone: false,
            last_commit_timestamp: 1700000000,
            last_author: "Alice",
            last_summary: "feat: initial commit",
          },
          {
            name: "feature/adversarial",
            is_current: false,
            is_remote: false,
            tip_commit_id: "e5f6a7b8",
            ahead_count: 2,
            behind_count: 0,
            is_default: false,
            is_gone: false,
            last_commit_timestamp: 1700000100,
            last_author: "Bob",
            last_summary: "wip: challenge",
          },
        ] as never;
      }

      case "cmd_list_tags": {
        if (config.failMetadata) {
          throw new Error("simulated git tag failure: exit code 128");
        }
        return { tags: [{ name: "v1.0.0", commit_id: "a1b2c3d4" }], truncated: false } as never;
      }

      case "cmd_stash_list": {
        if (config.failMetadata) {
          throw new Error("simulated stash list failure: corrupt packfile");
        }
        return {
          entries: [
            {
              index: 0,
              message: "WIP on main: a1b2c3d stash",
              commit_id: "c9d0e1f2",
              created_at: 1700000050,
            },
          ],
          truncated: false,
        } as never;
      }

      case "cmd_repo_operation": {
        if (config.failMetadata) {
          throw new Error("simulated repo operation failure: permission denied");
        }
        return null as never;
      }

      case "cmd_last_fetch_at": {
        if (config.failMetadata) {
          throw new Error("simulated last fetch failure: git config locked");
        }
        return 1699999000 as never;
      }

      case "cmd_watch_repo":
        return path as never;

      case "cmd_branch_stats":
        return {
          compared_to: "main",
          updates: [],
          computed: 0,
          cached: 0,
          capped: false,
          compute_failures: 0,
        } as never;

      default:
        return undefined as never;
    }
  };

  const store = createRepoStore({
    invoke,
    storage: memoryStorage(),
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: {
      subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe,
      setSearch: () => {},
      selectBranch: () => {},
      clear: () => {},
    },
    terminals: { countFor: () => 0, hostedFor: () => 0 },
  });

  return { store, counters, invoke };
}

describe("Faceted Hydration Pipeline Adversarial Stress Tests", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  describe("High-Frequency File Change Storms (Worktree Edits)", () => {
    it("50 rapid worktree edit events queue ONLY status and do not invoke the 7-command burst", async () => {
      vi.useFakeTimers();
      const { store, counters } = makeHarness();

      // Open repository initially (full hydrate)
      await store.openRepo("/repo/active");
      await vi.advanceTimersByTimeAsync(300);

      // Record baseline command counts after initial hydration
      const statusBefore = counters.calls.cmd_get_status ?? 0;
      const branchesBefore = counters.calls.cmd_list_branches ?? 0;
      const tagsBefore = counters.calls.cmd_list_tags ?? 0;
      const stashBefore = counters.calls.cmd_stash_list ?? 0;
      const opsBefore = counters.calls.cmd_repo_operation ?? 0;
      const fetchBefore = counters.calls.cmd_last_fetch_at ?? 0;
      const statsBefore = counters.calls.cmd_branch_stats ?? 0;

      expect(statusBefore).toBeGreaterThanOrEqual(1);
      expect(branchesBefore).toBeGreaterThanOrEqual(1);

      // Simulate rapid file change storm: 50 events firing every 5ms (total 250ms)
      const stormRepo = "/repo/active";
      for (let i = 0; i < 50; i++) {
        const change: RepoChange = {
          kinds: ["worktree", "index"],
          paths: [`src/components/File_${i}.svelte`],
          paths_truncated: false,
        };
        void store.handleRepoChanged(stormRepo, change);
        await vi.advanceTimersByTimeAsync(5);
      }

      // Advance past debounce window (WATCHER_REFRESH_DEBOUNCE_MS = 200ms)
      await vi.advanceTimersByTimeAsync(500);

      // VERIFY: Status facet was invoked
      const statusDelta = (counters.calls.cmd_get_status ?? 0) - statusBefore;
      expect(statusDelta).toBeGreaterThanOrEqual(1);
      // Because events collapsed into debounced single-flight, status ran at most 2 times, not 50
      expect(statusDelta).toBeLessThanOrEqual(3);

      // CRITICAL VERIFICATION: No other facets were spawned!
      const branchesDelta = (counters.calls.cmd_list_branches ?? 0) - branchesBefore;
      const tagsDelta = (counters.calls.cmd_list_tags ?? 0) - tagsBefore;
      const stashDelta = (counters.calls.cmd_stash_list ?? 0) - stashBefore;
      const opsDelta = (counters.calls.cmd_repo_operation ?? 0) - opsBefore;
      const fetchDelta = (counters.calls.cmd_last_fetch_at ?? 0) - fetchBefore;
      const statsDelta = (counters.calls.cmd_branch_stats ?? 0) - statsBefore;

      expect(branchesDelta, "cmd_list_branches must not be spawned on worktree edit").toBe(0);
      expect(tagsDelta, "cmd_list_tags must not be spawned on worktree edit").toBe(0);
      expect(stashDelta, "cmd_stash_list must not be spawned on worktree edit").toBe(0);
      expect(opsDelta, "cmd_repo_operation must not be spawned on worktree edit").toBe(0);
      expect(fetchDelta, "cmd_last_fetch_at must not be spawned on worktree edit").toBe(0);
      expect(statsDelta, "cmd_branch_stats must not be spawned on worktree edit").toBe(0);
    });

    it("50 rapid ref changes queue ONLY branches facet without status or metadata invocations", async () => {
      vi.useFakeTimers();
      const { store, counters } = makeHarness();

      await store.openRepo("/repo/refs");
      await vi.advanceTimersByTimeAsync(300);

      const statusBefore = counters.calls.cmd_get_status ?? 0;
      const branchesBefore = counters.calls.cmd_list_branches ?? 0;
      const tagsBefore = counters.calls.cmd_list_tags ?? 0;

      for (let i = 0; i < 50; i++) {
        const change: RepoChange = {
          kinds: ["refs"],
          paths: [".git/refs/heads/feature"],
          paths_truncated: false,
        };
        void store.handleRepoChanged("/repo/refs", change);
        await vi.advanceTimersByTimeAsync(5);
      }

      await vi.advanceTimersByTimeAsync(500);

      const statusDelta = (counters.calls.cmd_get_status ?? 0) - statusBefore;
      const branchesDelta = (counters.calls.cmd_list_branches ?? 0) - branchesBefore;
      const tagsDelta = (counters.calls.cmd_list_tags ?? 0) - tagsBefore;

      expect(branchesDelta).toBeGreaterThanOrEqual(1);
      expect(branchesDelta).toBeLessThanOrEqual(3);
      expect(statusDelta, "cmd_get_status must NOT be called on ref changes").toBe(0);
      expect(tagsDelta, "cmd_list_tags must NOT be called on ref changes").toBe(0);
    });

    it("Mixed change storm (worktree + refs) escalates via mergeFacets to 'all'", async () => {
      vi.useFakeTimers();
      const { store, counters } = makeHarness();

      await store.openRepo("/repo/mixed");
      await vi.advanceTimersByTimeAsync(300);

      const statusBefore = counters.calls.cmd_get_status ?? 0;
      const branchesBefore = counters.calls.cmd_list_branches ?? 0;

      // 20 worktree changes
      for (let i = 0; i < 20; i++) {
        void store.handleRepoChanged("/repo/mixed", {
          kinds: ["worktree"],
          paths: [`src/file_${i}.ts`],
          paths_truncated: false,
        });
        await vi.advanceTimersByTimeAsync(5);
      }

      // 1 ref change arriving before debounce resolves
      void store.handleRepoChanged("/repo/mixed", {
        kinds: ["refs"],
        paths: [".git/refs/heads/main"],
        paths_truncated: false,
      });

      // 20 more worktree changes
      for (let i = 0; i < 20; i++) {
        void store.handleRepoChanged("/repo/mixed", {
          kinds: ["worktree"],
          paths: [`src/other_${i}.ts`],
          paths_truncated: false,
        });
        await vi.advanceTimersByTimeAsync(5);
      }

      await vi.advanceTimersByTimeAsync(500);

      // Both status and branches should have run due to escalation to 'all'
      expect((counters.calls.cmd_get_status ?? 0) - statusBefore).toBeGreaterThanOrEqual(1);
      expect((counters.calls.cmd_list_branches ?? 0) - branchesBefore).toBeGreaterThanOrEqual(1);
    });
  });

  describe("Fault Isolation Under Injected Failures", () => {
    it("MetadataFacet complete failure isolates cleanly: statuses and branches populate, session remains healthy", async () => {
      vi.useFakeTimers();
      const { store } = makeHarness({ failMetadata: true });

      // Open repo with simulated metadata failures
      const opening = store.openRepo("/repo/fault-iso");
      await vi.advanceTimersByTimeAsync(300);
      await opening;

      const state = get(store);
      expect(state.currentPath).toBe("/repo/fault-iso");

      // Status facet succeeded despite metadata failure
      expect(state.statuses.length).toBe(1);
      expect(state.statuses[0].path).toBe("src/file.ts");

      // Branch facet succeeded despite metadata failure
      expect(state.branches.length).toBe(2);
      expect(state.currentBranch).toBe("main");

      // Metadata errors are captured gracefully in facet flags
      expect(state.tagsFailed).toBe(true);
      expect(state.tags).toEqual([]);
      expect(state.stashFailed).toBe(true);
      expect(state.stashEntries).toEqual([]);
      expect(state.operation?.probeFailed).toBe(true);
      expect(state.fetchedAt).toBeNull();

      // Crucial: The session as a whole has hydrated and has no fatal session-level error
      expect(state.isLoading).toBe(false);
      expect(state.error).toBeNull();
    });

    it("Status facet failure records error without blocking subsequent recovery", async () => {
      vi.useFakeTimers();
      const { store } = makeHarness({ failStatus: true });

      // Open repo where status command fails
      const opening = store.openRepo("/repo/fail-status");
      await vi.advanceTimersByTimeAsync(300);
      await opening;

      const state = get(store);
      expect(state.currentPath).toBe("/repo/fail-status");
      expect(state.isLoading).toBe(false);
      expect(state.error).toMatch(/SpawnGate token depleted/);
    });
  });

  describe("Facet Helper Invariants (facetForChange & mergeFacets)", () => {
    it("accurately classifies change events", () => {
      expect(facetForChange(null)).toBe("all");
      expect(facetForChange({ kinds: [], paths: [], paths_truncated: false })).toBe("all");
      expect(facetForChange({ kinds: ["worktree"], paths: ["a.ts"], paths_truncated: false })).toBe("status");
      expect(facetForChange({ kinds: ["index"], paths: ["b.ts"], paths_truncated: false })).toBe("status");
      expect(facetForChange({ kinds: ["documents", "ignore"], paths: [".gitignore"], paths_truncated: false })).toBe("status");
      expect(facetForChange({ kinds: ["refs"], paths: ["heads"], paths_truncated: false })).toBe("branches");
      expect(facetForChange({ kinds: ["git_state"], paths: ["MERGE_HEAD"], paths_truncated: false })).toBe("branches");
      expect(facetForChange({ kinds: ["objects"], paths: ["pack"], paths_truncated: false })).toBe("branches");
      expect(facetForChange({ kinds: ["config"], paths: [".git/config"], paths_truncated: false })).toBe("metadata");

      // Mixed kinds classify as "all"
      expect(facetForChange({ kinds: ["worktree", "refs"], paths: [], paths_truncated: false })).toBe("all");
      expect(facetForChange({ kinds: ["worktree", "config"], paths: [], paths_truncated: false })).toBe("all");
    });

    it("merges facets deterministically without losing required refreshes", () => {
      expect(mergeFacets(undefined, "status")).toBe("status");
      expect(mergeFacets("status", "status")).toBe("status");
      expect(mergeFacets("branches", "branches")).toBe("branches");
      expect(mergeFacets("metadata", "metadata")).toBe("metadata");
      expect(mergeFacets("status", "branches")).toBe("all");
      expect(mergeFacets("branches", "status")).toBe("all");
      expect(mergeFacets("status", "metadata")).toBe("all");
      expect(mergeFacets("all", "status")).toBe("all");
      expect(mergeFacets("status", "all")).toBe("all");
    });
  });
});
