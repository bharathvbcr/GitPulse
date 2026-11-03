import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import type { BranchInfo } from "../../branches/types";
import { memoryStorage } from "../../repos/persist";

type BranchFixtureConfig = {
  branches: BranchInfo[];
  statsComparedTo?: string;
  statsUpdates?: Array<{
    name: string;
    is_remote: boolean;
    remote_name?: string;
    tip_commit_id: string;
    additions: number;
    deletions: number;
    files_changed: number;
    commits_ahead_of_base: number;
    commits_behind_base: number;
  }>;
};

function makeBranch(
  overrides: Partial<BranchInfo> & Pick<BranchInfo, "name" | "tip_commit_id">,
): BranchInfo {
  return {
    is_current: false,
    is_remote: false,
    ahead_count: 0,
    behind_count: 0,
    is_default: false,
    is_gone: false,
    last_commit_timestamp: 1700000000,
    last_author: "Alice",
    last_summary: "commit",
    commits_ahead_of_base: 0,
    commits_behind_base: 0,
    additions: 0,
    deletions: 0,
    files_changed: 0,
    ...overrides,
  };
}

function makeBranchHarness(config: BranchFixtureConfig) {
  const calls: Record<string, number> = {};
  let currentBranches = config.branches.map((b) => ({ ...b }));

  const invoke: InvokeFn = async (cmd, args) => {
    calls[cmd] = (calls[cmd] ?? 0) + 1;
    const path = String(args?.repoPath ?? "/repo");

    switch (cmd) {
      case "cmd_resolve_repo":
        return {
          path,
          name: "repo",
          is_bare: false,
          common_dir: `${path}/.git`,
        } as never;

      case "cmd_get_status":
        return [] as never;

      case "cmd_list_branches":
        return currentBranches.map((b) => ({ ...b })) as never;

      case "cmd_branch_stats":
        return {
          compared_to: config.statsComparedTo ?? "main",
          updates: config.statsUpdates ?? [],
          computed: config.statsUpdates?.length ?? 0,
          cached: 0,
          capped: false,
          compute_failures: 0,
        } as never;

      case "cmd_list_tags":
        return { tags: [], truncated: false } as never;

      case "cmd_stash_list":
        return { entries: [], truncated: false } as never;

      case "cmd_repo_operation":
        return null as never;

      case "cmd_last_fetch_at":
        return null as never;

      case "cmd_create_branch": {
        const branchName = String(args?.branchName ?? "");
        const startPoint = args?.startPoint ? String(args.startPoint) : undefined;
        const donor = startPoint
          ? currentBranches.find((b) => b.name === startPoint || b.tip_commit_id === startPoint)
          : currentBranches.find((b) => b.is_current);
        const tip = donor?.tip_commit_id ?? startPoint ?? "deadbeef";
        currentBranches = [
          ...currentBranches,
          makeBranch({
            name: branchName,
            tip_commit_id: tip,
            last_commit_timestamp: 1700000200,
            last_summary: "branch tip",
          }),
        ];
        return null as never;
      }

      case "cmd_rename_branch": {
        const oldName = String(args?.oldName ?? "");
        const newName = String(args?.newName ?? "");
        currentBranches = currentBranches.map((b) =>
          !b.is_remote && b.name === oldName ? { ...b, name: newName } : b,
        );
        return null as never;
      }

      case "cmd_delete_branch": {
        const branchName = String(args?.branchName ?? "");
        currentBranches = currentBranches.filter((b) => b.is_remote || b.name !== branchName);
        return null as never;
      }

      case "cmd_watch_repo":
      case "cmd_unwatch_repo":
      case "cmd_list_commits":
        return [] as never;

      default:
        return null as never;
    }
  };

  return {
    calls,
    getBranches: () => currentBranches,
    setBranches: (next: BranchInfo[]) => {
      currentBranches = next.map((b) => ({ ...b }));
    },
    resetCounts: () => {
      for (const k of Object.keys(calls)) delete calls[k];
    },
    invoke,
  };
}

describe("repoStore branch ref mutations (faceted refresh & tip-matched churn inheritance)", () => {
  it("refreshes only the branches facet on createBranch, renameBranch, and deleteBranch once hydrated", async () => {
    const harness = makeBranchHarness({
      branches: [
        makeBranch({
          name: "main",
          is_current: true,
          tip_commit_id: "1111111111111111111111111111111111111111",
          is_default: true,
          last_summary: "init",
        }),
        makeBranch({
          name: "feat/existing",
          tip_commit_id: "2222222222222222222222222222222222222222",
          ahead_count: 2,
          last_commit_timestamp: 1700000100,
          last_author: "Bob",
          last_summary: "feat work",
        }),
      ],
      statsComparedTo: "main",
      statsUpdates: [
        {
          name: "feat/existing",
          is_remote: false,
          tip_commit_id: "2222222222222222222222222222222222222222",
          additions: 42,
          deletions: 7,
          files_changed: 3,
          commits_ahead_of_base: 2,
          commits_behind_base: 0,
        },
      ],
    });

    const store = createRepoStore({
      invoke: harness.invoke,
      storage: memoryStorage(),
    });

    await store.openRepo("/repo");
    // Wait a microtask tick for initial background stats drain to settle.
    await new Promise((r) => setTimeout(r, 15));
    expect(get(store).isLoading).toBe(false);

    harness.resetCounts();

    const created = await store.createBranch("feat/from-existing", "feat/existing");
    expect(created.ok).toBe(true);
    await new Promise((r) => setTimeout(r, 15));
    expect(harness.calls.cmd_create_branch).toBe(1);
    expect(harness.calls.cmd_list_branches).toBe(1);
    // Pure ref creation with tip-matched verified donor churn must NOT trigger
    // working-tree status, metadata scans, OR a redundant `cmd_branch_stats` walk.
    expect(harness.calls.cmd_branch_stats ?? 0).toBe(0);
    expect(harness.calls.cmd_get_status ?? 0).toBe(0);
    expect(harness.calls.cmd_list_tags ?? 0).toBe(0);
    expect(harness.calls.cmd_stash_list ?? 0).toBe(0);
    expect(harness.calls.cmd_repo_operation ?? 0).toBe(0);
    expect(harness.calls.cmd_last_fetch_at ?? 0).toBe(0);

    // Churn stats are inherited immediately from the tip-matched donor branch.
    const fromExisting = get(store).branches.find((b) => b.name === "feat/from-existing");
    expect(fromExisting).toBeDefined();
    expect(fromExisting?.compared_to).toBe("main");
    expect(fromExisting?.additions).toBe(42);
    expect(fromExisting?.deletions).toBe(7);
    expect(fromExisting?.files_changed).toBe(3);
    expect(fromExisting?.commits_ahead_of_base).toBe(2);

    // Creating a branch directly at `main`'s tip inherits zero-diff churn against `main`
    // and also skips `cmd_branch_stats`.
    harness.resetCounts();
    const fromMain = await store.createBranch("feat/from-main", "main");
    expect(fromMain.ok).toBe(true);
    await new Promise((r) => setTimeout(r, 15));
    expect(harness.calls.cmd_branch_stats ?? 0).toBe(0);
    expect(harness.calls.cmd_get_status ?? 0).toBe(0);
    const mainClone = get(store).branches.find((b) => b.name === "feat/from-main");
    expect(mainClone?.compared_to).toBe("main");
    expect(mainClone?.additions).toBe(0);
    expect(mainClone?.deletions).toBe(0);
    expect(mainClone?.commits_ahead_of_base).toBe(0);

    // Renaming preserves churn across the name change without scanning working tree or stats.
    harness.resetCounts();
    const renamed = await store.renameBranch("feat/from-existing", "feat/renamed");
    expect(renamed.ok).toBe(true);
    await new Promise((r) => setTimeout(r, 15));
    expect(harness.calls.cmd_rename_branch).toBe(1);
    expect(harness.calls.cmd_branch_stats ?? 0).toBe(0);
    expect(harness.calls.cmd_get_status ?? 0).toBe(0);
    const renamedBranch = get(store).branches.find((b) => b.name === "feat/renamed");
    expect(renamedBranch?.additions).toBe(42);
    expect(renamedBranch?.compared_to).toBe("main");

    // Deleting refreshes only branches and skips `cmd_branch_stats` when remaining branches are measured.
    harness.resetCounts();
    const deleted = await store.deleteBranch("feat/renamed");
    expect(deleted.ok).toBe(true);
    await new Promise((r) => setTimeout(r, 15));
    expect(harness.calls.cmd_delete_branch).toBe(1);
    expect(harness.calls.cmd_branch_stats ?? 0).toBe(0);
    expect(harness.calls.cmd_get_status ?? 0).toBe(0);
    expect(get(store).branches.find((b) => b.name === "feat/renamed")).toBeUndefined();
  });

  it("still invokes cmd_branch_stats when creating at an unmeasured SHA or unshadowing a diverged remote", async () => {
    const harness = makeBranchHarness({
      branches: [
        makeBranch({
          name: "main",
          is_current: true,
          tip_commit_id: "1111111111111111111111111111111111111111",
          is_default: true,
        }),
        makeBranch({
          name: "feat/local-twin",
          tip_commit_id: "2222222222222222222222222222222222222222",
        }),
        makeBranch({
          name: "origin/feat/local-twin",
          is_remote: true,
          remote_name: "origin",
          tip_commit_id: "3333333333333333333333333333333333333333",
          compared_to: "main",
        }),
      ],
      statsComparedTo: "main",
      statsUpdates: [
        {
          name: "feat/local-twin",
          is_remote: false,
          tip_commit_id: "2222222222222222222222222222222222222222",
          additions: 10,
          deletions: 2,
          files_changed: 1,
          commits_ahead_of_base: 1,
          commits_behind_base: 0,
        },
      ],
    });

    const store = createRepoStore({
      invoke: harness.invoke,
      storage: memoryStorage(),
    });

    await store.openRepo("/repo");
    await new Promise((r) => setTimeout(r, 15));

    // 1. Creating a branch at an arbitrary historical SHA with no donor must trigger `cmd_branch_stats`.
    harness.resetCounts();
    await store.createBranch(
      "feat/arbitrary-sha",
      "9999999999999999999999999999999999999999",
    );
    await new Promise((r) => setTimeout(r, 15));
    expect(harness.calls.cmd_branch_stats).toBe(1);

    // 2. Deleting `feat/local-twin` unshadows `origin/feat/local-twin` (at unmeasured tip `3333...`),
    // which `cmd_branch_stats` had skipped while shadowed — so `cmd_branch_stats` MUST run.
    harness.resetCounts();
    await store.deleteBranch("feat/local-twin");
    await new Promise((r) => setTimeout(r, 15));
    expect(harness.calls.cmd_branch_stats).toBe(1);
  });

  it("refuses to carry stale tip-matched churn if the compared_to base ref moved concurrently", async () => {
    const harness = makeBranchHarness({
      branches: [
        makeBranch({
          name: "main",
          is_current: true,
          tip_commit_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          is_default: true,
          last_summary: "init",
        }),
        makeBranch({
          name: "feat/donor",
          tip_commit_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          ahead_count: 1,
          last_commit_timestamp: 1700000100,
          last_author: "Bob",
          last_summary: "donor",
        }),
      ],
      statsComparedTo: "main",
      statsUpdates: [
        {
          name: "feat/donor",
          is_remote: false,
          tip_commit_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
          additions: 19,
          deletions: 3,
          files_changed: 2,
          commits_ahead_of_base: 1,
          commits_behind_base: 0,
        },
      ],
    });

    const customInvoke: InvokeFn = async <T>(
      cmd: string,
      args?: Record<string, unknown>,
    ): Promise<T> => {
      if (cmd === "cmd_create_branch") {
        const res = await harness.invoke<T>(cmd, args);
        // Simulate `main` advancing concurrently before `cmd_list_branches` runs!
        harness.setBranches(
          harness.getBranches().map((b) =>
            b.name === "main"
              ? { ...b, tip_commit_id: "cccccccccccccccccccccccccccccccccccccccc" }
              : b,
          ),
        );
        return res;
      }
      if (cmd === "cmd_branch_stats" && (harness.calls.cmd_create_branch ?? 0) > 0) {
        // Hold post-create stats drain long enough to inspect the synchronous snapshot state.
        await new Promise((r) => setTimeout(r, 80));
        return {
          compared_to: "main",
          updates: [],
          computed: 0,
          cached: 0,
          capped: false,
          compute_failures: 0,
        } as T;
      }
      return harness.invoke<T>(cmd, args);
    };

    const store = createRepoStore({
      invoke: customInvoke,
      storage: memoryStorage(),
    });

    await store.openRepo("/repo");
    await new Promise((r) => setTimeout(r, 15));
    expect(
      get(store).branches.find((b) => b.name === "feat/donor")?.additions,
    ).toBe(19);

    await store.createBranch("feat/new-after-base-move", "feat/donor");
    const created = get(store).branches.find((b) => b.name === "feat/new-after-base-move");
    const existingDonor = get(store).branches.find((b) => b.name === "feat/donor");
    expect(created).toBeDefined();
    // Because `main` moved from `aaa...` to `ccc...`, BOTH the new branch AND existing
    // `feat/donor` must drop stale churn measured against old `main@aaa...`.
    expect(created?.compared_to).toBeUndefined();
    expect(created?.additions).toBe(0);
    expect(existingDonor?.compared_to).toBeUndefined();
    expect(existingDonor?.additions).toBe(0);
  });

  it("ignores unmeasured 0/0/0 churn on shadowed remote branches and while statsPending is true", async () => {
    let releaseInitialStats!: () => void;
    const initialStatsGate = new Promise<void>((resolve) => {
      releaseInitialStats = resolve;
    });

    const harness = makeBranchHarness({
      branches: [
        makeBranch({
          name: "main",
          is_current: true,
          tip_commit_id: "1111111111111111111111111111111111111111",
          is_default: true,
          last_summary: "init",
        }),
        // `cmd_list_branches` returns remote branches with placeholder `0/0/0` + `compared_to: "main"`,
        // and `cmd_branch_stats` skips remote branches shadowed by a local twin (`git_reader.rs:880`).
        makeBranch({
          name: "origin/feat/existing",
          is_remote: true,
          remote_name: "origin",
          tip_commit_id: "2222222222222222222222222222222222222222",
          compared_to: "main",
          last_commit_timestamp: 1700000100,
          last_author: "Bob",
          last_summary: "feat work",
        }),
        makeBranch({
          name: "feat/existing",
          tip_commit_id: "2222222222222222222222222222222222222222",
          ahead_count: 2,
          compared_to: "main",
          last_commit_timestamp: 1700000100,
          last_author: "Bob",
          last_summary: "feat work",
        }),
      ],
      statsComparedTo: "main",
      statsUpdates: [
        {
          name: "feat/existing",
          is_remote: false,
          tip_commit_id: "2222222222222222222222222222222222222222",
          additions: 42,
          deletions: 7,
          files_changed: 3,
          commits_ahead_of_base: 2,
          commits_behind_base: 0,
        },
      ],
    });

    let initialStatsPending = true;
    const customInvoke: InvokeFn = async <T>(
      cmd: string,
      args?: Record<string, unknown>,
    ): Promise<T> => {
      if (cmd === "cmd_branch_stats") {
        if (initialStatsPending) {
          await initialStatsGate;
          initialStatsPending = false;
          return harness.invoke<T>(cmd, args);
        }
        await new Promise((r) => setTimeout(r, 80));
        return {
          compared_to: "main",
          updates: [],
          computed: 0,
          cached: 0,
          capped: false,
          compute_failures: 0,
        } as T;
      }
      return harness.invoke<T>(cmd, args);
    };

    const store = createRepoStore({
      invoke: customInvoke,
      storage: memoryStorage(),
    });

    await store.openRepo("/repo");
    expect(get(store).statsPending).toBe(true);

    // Creating a branch while `statsPending` is true MUST NOT copy unmeasured `0/0/0` placeholder churn.
    await store.createBranch("feat/while-pending", "feat/existing");
    const whilePending = get(store).branches.find((b) => b.name === "feat/while-pending");
    expect(whilePending?.compared_to).toBeUndefined();

    // Now let initial `cmd_branch_stats` finish; `feat/existing` has `42/7/3` while `origin/feat/existing`
    // earlier in the array still has unmeasured `0/0/0`.
    releaseInitialStats();
    await new Promise((r) => setTimeout(r, 20));
    expect(get(store).statsPending).toBe(false);
    expect(get(store).branches.find((b) => b.name === "feat/existing")?.additions).toBe(42);

    await store.createBranch("feat/from-shadowed", "feat/existing");
    const fromShadowed = get(store).branches.find((b) => b.name === "feat/from-shadowed");
    expect(fromShadowed?.compared_to).toBe("main");
    expect(fromShadowed?.additions).toBe(42);
    expect(fromShadowed?.deletions).toBe(7);
    expect(fromShadowed?.files_changed).toBe(3);
  });

  it("populates 0/0/0 churn when creating the first branch off default in a single-branch repo", async () => {
    const harness = makeBranchHarness({
      branches: [
        makeBranch({
          name: "main",
          is_current: true,
          tip_commit_id: "1111111111111111111111111111111111111111",
          is_default: true,
          last_summary: "init",
        }),
      ],
      statsComparedTo: "main",
      statsUpdates: [],
    });

    const customInvoke: InvokeFn = async <T>(
      cmd: string,
      args?: Record<string, unknown>,
    ): Promise<T> => {
      if (cmd === "cmd_branch_stats" && (harness.calls.cmd_create_branch ?? 0) > 0) {
        await new Promise((r) => setTimeout(r, 80));
        return {
          compared_to: "main",
          updates: [],
          computed: 0,
          cached: 0,
          capped: false,
          compute_failures: 0,
        } as T;
      }
      return harness.invoke<T>(cmd, args);
    };

    const store = createRepoStore({
      invoke: customInvoke,
      storage: memoryStorage(),
    });

    await store.openRepo("/repo");
    await new Promise((r) => setTimeout(r, 15));
    expect(get(store).statsPending).toBe(false);

    await store.createBranch("feat/first-branch");
    const first = get(store).branches.find((b) => b.name === "feat/first-branch");
    expect(first?.compared_to).toBe("main");
    expect(first?.additions).toBe(0);
    expect(first?.deletions).toBe(0);
    expect(first?.files_changed).toBe(0);
  });

  it("remembers drained report.compared_to and skips cmd_branch_stats when creating the first branch in a single-branch non-default repo", async () => {
    const harness = makeBranchHarness({
      branches: [
        makeBranch({
          name: "trunk",
          is_current: true,
          tip_commit_id: "1111111111111111111111111111111111111111",
          is_default: false,
          last_summary: "init",
        }),
      ],
      statsComparedTo: "trunk",
      statsUpdates: [],
    });

    const store = createRepoStore({
      invoke: harness.invoke,
      storage: memoryStorage(),
    });

    await store.openRepo("/repo");
    await new Promise((r) => setTimeout(r, 15));
    expect(get(store).statsPending).toBe(false);

    harness.resetCounts();
    await store.createBranch("feat/first-from-trunk");
    await new Promise((r) => setTimeout(r, 15));
    expect(harness.calls.cmd_branch_stats ?? 0).toBe(0);
    const first = get(store).branches.find((b) => b.name === "feat/first-from-trunk");
    expect(first?.compared_to).toBe("trunk");
    expect(first?.additions).toBe(0);
  });
});
