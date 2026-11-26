import { beforeEach, describe, expect, it, vi } from "vitest";
import { writable } from "svelte/store";

const invoke = vi.fn();
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("../stores/modalStore", () => ({
  askConfirm: () => {
    throw new Error("an unattended batch merge must not ask");
  },
}));

const repoState = writable<{ openTabs: unknown[] }>({ openTabs: [] });
const trustRepo = vi.fn(async (path: string): Promise<string | null> => path);
const closeTab = vi.fn(async (_id: string) => {});
let fake: ReturnType<typeof import("./attemptRepoStoreFake").fakeAttemptStore>;

vi.mock("../stores/repoStore", async () => {
  const { fakeAttemptStore } = await import("./attemptRepoStoreFake");
  fake = fakeAttemptStore((command, args) => invoke(command, args), {
    merge: "cmd_worktree_merge_teardown",
    remove: "cmd_remove_worktree",
    deleteBranch: "cmd_delete_branch",
  });
  return {
    repoStore: {
      subscribe: repoState.subscribe,
      trustRepo: (p: string) => trustRepo(p),
      closeTab: (id: string) => closeTab(id),
      previewUncommitted: async () => {},
      ...fake.members,
    },
  };
});
vi.mock("../stores/interfaceStore", () => ({ interfaceStore: { setGlobalSurface: () => {} } }));
const live = vi.fn(async (): Promise<{ runs: { cwd: string }[]; complete: boolean }> => ({
  runs: [],
  complete: true,
}));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  listAllLiveRuns: () => live(),
}));

const { mergeVerifiedAttemptBatch } = await import("./attemptWorktree");
const { MergeTeardownFailure } = await import("../branches/mergeTeardown");

const WT_A = "/work/repo/.gitpulse/worktrees/lane-a";
const WT_B = "/work/repo/.gitpulse/worktrees/lane-b";
const entry = (over: Record<string, unknown>) => ({
  name: "n",
  is_bare: false,
  is_detached: false,
  is_locked: false,
  is_prunable: false,
  diff_stat: null,
  main_divergence: null,
  active_routes: [],
  ...over,
});

beforeEach(() => {
  invoke.mockReset();
  closeTab.mockClear();
  trustRepo.mockClear();
  trustRepo.mockImplementation(async (p) => p);
  live.mockReset();
  live.mockResolvedValue({ runs: [], complete: true });
  repoState.set({ openTabs: [] });
});

describe("mergeVerifiedAttemptBatch", () => {
  it("submits multiple verified worktrees through cmd_worktree_merge_batch and isolates per-lane failures", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_resolve_repo") {
        return { path: WT_A, name: "repo", is_bare: false, common_dir: "/work/repo/.git" };
      }
      if (cmd === "cmd_list_worktrees") {
        return [
          entry({ path: "/work/repo", head: "main0", branch: "main", is_main: true, dirty_files: 0 }),
          entry({ path: WT_A, head: "headA", branch: "gitpulse/a", is_main: false, dirty_files: 0 }),
          entry({ path: WT_B, head: "headB", branch: "gitpulse/b", is_main: false, dirty_files: 0 }),
        ];
      }
      if (cmd === "cmd_worktree_merge_batch") {
        return {
          target_branch: "main",
          before_oid: "main0",
          after_oid: "main1",
          policy: { status: "allowed", checked: true },
          outcomes: [
            {
              branch: "gitpulse/a",
              worktree_path: WT_A,
              ok: true,
              already_landed: false,
              source_oid: "headA",
              landed_oid: "main1",
              before_oid: "main0",
              batch_tip: "main1",
              checkout_advanced: true,
              checkout_note: null,
              green_ref_note: null,
              worktree_removed: true,
              removal_error: null,
              branch_deleted: true,
              branch_error: null,
              error_code: null,
              error: null,
              conflict_paths: [],
              held_reason: null,
            },
            {
              branch: "gitpulse/b",
              worktree_path: WT_B,
              ok: false,
              already_landed: false,
              source_oid: "headB",
              landed_oid: null,
              before_oid: "main0",
              batch_tip: null,
              checkout_advanced: false,
              checkout_note: null,
              green_ref_note: null,
              worktree_removed: false,
              removal_error: null,
              branch_deleted: false,
              branch_error: null,
              error_code: "conflict",
              error: "Conflict in shared.ts",
              conflict_paths: ["shared.ts"],
              held_reason: null,
            },
          ],
        };
      }
      throw new Error(`unexpected ${cmd}`);
    });

    repoState.set({ openTabs: [{ id: "tab-a", path: WT_A }, { id: "tab-b", path: WT_B }] });
    const results = await mergeVerifiedAttemptBatch([
      { run: { id: "r1", cwd: WT_A, state: "exited", expires_at: 0 }, expectedHead: "headA", taskId: "t1" },
      { run: { id: "r2", cwd: WT_B, state: "exited", expires_at: 0 }, expectedHead: "headB", taskId: "t2" },
    ]);

    expect(results[0]).toEqual({
      ok: true,
      merged: {
        head: "headA",
        message: "Merged gitpulse/a into main. The worktree was removed.",
        beforeOid: "main0",
        afterOid: "main1",
      },
    });
    expect(results[1].ok).toBe(false);
    if (!results[1].ok) {
      expect(results[1].cause).toBeInstanceOf(MergeTeardownFailure);
      expect((results[1].cause as InstanceType<typeof MergeTeardownFailure>).failure.code).toBe("conflict");
    }
    expect(closeTab).toHaveBeenCalledTimes(1);
    expect(closeTab).toHaveBeenCalledWith("tab-a");
    expect(invoke).toHaveBeenCalledWith("cmd_worktree_merge_batch", {
      repoPath: "/work/repo",
      targetBranch: "main",
      lanes: [
        {
          worktree_path: WT_A,
          branch: "gitpulse/a",
          source_oid: "headA",
          squash: false,
          commit_message: null,
          hot_files: null,
          verify_in_scratch: true,
        },
        {
          worktree_path: WT_B,
          branch: "gitpulse/b",
          source_oid: "headB",
          squash: false,
          commit_message: null,
          hot_files: null,
          verify_in_scratch: true,
        },
      ],
      removeWorktrees: true,
    });
  });

  it("falls back to single-item cmd_worktree_merge_teardown when given 1 item or custom verifier hooks", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_resolve_repo") {
        return { path: WT_A, name: "repo", is_bare: false, common_dir: "/work/repo/.git" };
      }
      if (cmd === "cmd_list_worktrees") {
        return [
          entry({ path: "/work/repo", head: "main0", branch: "main", is_main: true, dirty_files: 0 }),
          entry({ path: WT_A, head: "headA", branch: "gitpulse/a", is_main: false, dirty_files: 0 }),
        ];
      }
      if (cmd === "cmd_worktree_merge_teardown") {
        return {
          policy: null,
          output: {
            merged: true,
            merged_branch: "gitpulse/a",
            target_branch: "main",
            commits_merged: 1,
            before_oid: "main0",
            after_oid: "main1",
            base_oid: null,
            worktree_removed: true,
            removal_error: null,
            branch_deleted: true,
            branch_error: null,
            hook_error: null,
          },
        };
      }
      throw new Error(`unexpected ${cmd}`);
    });

    const single = await mergeVerifiedAttemptBatch([
      { run: { id: "r1", cwd: WT_A, state: "exited", expires_at: 0 }, expectedHead: "headA", taskId: "t1" },
    ]);
    expect(single).toHaveLength(1);
    expect(single[0].ok).toBe(true);
    expect(invoke.mock.calls.map((c) => c[0])).toContain("cmd_worktree_merge_teardown");
    expect(invoke.mock.calls.map((c) => c[0])).not.toContain("cmd_worktree_merge_batch");
  });
});
