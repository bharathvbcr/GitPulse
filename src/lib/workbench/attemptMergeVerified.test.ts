import { beforeEach, describe, expect, it, vi } from "vitest";
import { writable } from "svelte/store";

const invoke = vi.fn();
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("../stores/modalStore", () => ({ askConfirm: () => { throw new Error("an unattended merge must not ask"); } }));
const repoState = writable<{ openTabs: unknown[] }>({ openTabs: [] });
const trustRepo = vi.fn(async (path: string): Promise<string | null> => path);
const closeTab = vi.fn(async (_id: string) => {});
let fake: ReturnType<typeof import("./attemptRepoStoreFake").fakeAttemptStore>;
vi.mock("../stores/repoStore", async () => {
  const { fakeAttemptStore } = await import("./attemptRepoStoreFake");
  fake = fakeAttemptStore((command, args) => invoke(command, args), { merge: "cmd_worktree_merge_teardown", remove: "cmd_remove_worktree", deleteBranch: "cmd_delete_branch" });
  return { repoStore: { subscribe: repoState.subscribe, trustRepo: (p: string) => trustRepo(p), closeTab: (id: string) => closeTab(id), previewUncommitted: async () => {}, ...fake.members } };
});
vi.mock("../stores/interfaceStore", () => ({ interfaceStore: { setGlobalSurface: () => {} } }));
const live = vi.fn(async (): Promise<{ runs: { cwd: string }[]; complete: boolean }> => ({ runs: [], complete: true }));
vi.mock("./client", async (original) => ({ ...(await original<typeof import("./client")>()), listAllLiveRuns: () => live() }));

const { mergeVerifiedAttempt } = await import("./attemptWorktree");
const { MergeTeardownFailure } = await import("../branches/mergeTeardown");

const WT = "/work/repo/.gitpulse/worktrees/fix-e42-wt0attem";
const run = { id: "wt0attempt", cwd: WT, state: "exited" as const, expires_at: 0 };
/** `cmd_worktree_merge_teardown`'s output for a merge that landed and cleaned up, unless `over` says otherwise. */
const teardown = (over: Record<string, unknown> = {}) => ({ merged: true, merged_branch: "gitpulse/fix", target_branch: "main", commits_merged: 1, before_oid: "a", after_oid: "1234567890abcdef", base_oid: null, worktree_removed: true, removal_error: null, branch_deleted: true, branch_error: null, hook_error: null, ...over });
const entry = (over: Record<string, unknown>) => ({ name: "n", is_bare: false, is_detached: false, is_locked: false, is_prunable: false, diff_stat: null, main_divergence: null, active_routes: [], ...over });
function setup(own: Record<string, unknown> = {}, main: Record<string, unknown> = {}) {
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === "cmd_resolve_repo") return { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" };
    if (cmd === "cmd_list_worktrees") return [
      entry({ path: "/work/repo", head: "mainhead", branch: "main", is_main: true, dirty_files: 0, ...main }),
      entry({ path: WT, head: "1234567890abcdef", branch: "gitpulse/fix", is_main: false, dirty_files: 0, ...own }),
    ];
    if (cmd === "cmd_worktree_merge_teardown") return { policy: null, output: teardown() };
    throw new Error(`unexpected ${cmd}`);
  });
}
const merged = () => invoke.mock.calls.some((c) => c[0] === "cmd_worktree_merge_teardown");

beforeEach(() => { invoke.mockReset(); closeTab.mockClear(); trustRepo.mockClear(); trustRepo.mockImplementation(async (p) => p); fake.seen.length = 0; fake.opened.length = 0; fake.state.mainOpen = true; fake.state.stillNamed = () => true; live.mockReset(); live.mockResolvedValue({ runs: [], complete: true }); repoState.set({ openTabs: [] }); });

describe("mergeVerifiedAttempt", () => {
  it("merges the verified head without asking, into the main checkout's branch, and never pushes", async () => {
    setup();
    const result = await mergeVerifiedAttempt(run, "1234567890abcdef");
    expect(result).toEqual({ head: "1234567890abcdef", message: "Merged 1 commit from gitpulse/fix into main. The worktree was removed." });
    // The head the verdict was reached for is the tip the host holds the merge to.
    expect(invoke).toHaveBeenLastCalledWith("cmd_worktree_merge_teardown", { repoPath: "/work/repo", worktreePath: WT, targetBranch: "main", squash: false, expectedTip: "1234567890abcdef" });
    expect(invoke.mock.calls.map((c) => String(c[0])).filter((c) => /push|remote/.test(c))).toEqual([]);
  });

  it("adv: carries the target's tip before and after the merge, and a null after-oid as null, never as the head", async () => {
    setup();
    const full = await mergeVerifiedAttempt(run, "1234567890abcdef");
    expect([full.beforeOid, full.afterOid]).toEqual(["a", "1234567890abcdef"]);
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_resolve_repo") return { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" };
      if (cmd === "cmd_list_worktrees") return [
        entry({ path: "/work/repo", head: "mainhead", branch: "main", is_main: true, dirty_files: 0 }),
        entry({ path: WT, head: "1234567890abcdef", branch: "gitpulse/fix", is_main: false, dirty_files: 0 }),
      ];
      return { policy: null, output: teardown({ before_oid: "b0", after_oid: null }) };
    });
    const unknown = await mergeVerifiedAttempt(run, "1234567890abcdef");
    expect([unknown.beforeOid, unknown.afterOid]).toEqual(["b0", null]);
  });

  it.each([
    ["the worktree moved past the verified head", { head: "fedcba0987654321" }, {}, /moved to fedcba09 after 12345678/],
    ["the worktree has uncommitted files", { dirty_files: 2 }, {}, /2 uncommitted files no verdict covered/],
    ["the worktree's changes could not be counted", { dirty_files: null }, {}, /could not be counted/],
    ["the main checkout is dirty", {}, { dirty_files: 1 }, /main checkout \/work\/repo has 1 uncommitted file/],
    ["the main checkout could not be checked", {}, { dirty_files: null }, /could not be checked for uncommitted work/],
    ["the main checkout is detached", {}, { branch: null }, /not on a branch/],
    ["the attempt has no branch", { branch: null }, {}, /not on a branch/],
  ])("merges nothing when %s", async (_why, own, main, message) => {
    setup(own, main);
    await expect(mergeVerifiedAttempt(run, "1234567890abcdef")).rejects.toThrow(message);
    expect(merged()).toBe(false);
  });

  it("merges nothing when another attempt works in the main checkout, or live attempts could not all be read", async () => {
    setup();
    live.mockResolvedValue({ runs: [{ cwd: "/work/repo" }], complete: true });
    await expect(mergeVerifiedAttempt(run, "1234567890abcdef")).rejects.toThrow(/Another attempt is working in the main checkout/);
    live.mockResolvedValue({ runs: [], complete: false });
    await expect(mergeVerifiedAttempt(run, "1234567890abcdef")).rejects.toThrow(/could not all be read/);
    live.mockRejectedValue(new Error("store down"));
    await expect(mergeVerifiedAttempt(run, "1234567890abcdef")).rejects.toThrow(/store down/);
    expect(merged()).toBe(false);
  });

  it("merges nothing for an empty head, an untrusted repository, or an attempt that ran in the main checkout", async () => {
    setup();
    await expect(mergeVerifiedAttempt(run, "")).rejects.toThrow(/No verified head/);
    trustRepo.mockResolvedValue(null);
    await expect(mergeVerifiedAttempt(run, "1234567890abcdef")).rejects.toThrow(/not trusted/);
    invoke.mockImplementation(async () => ({ path: "/work/repo", name: "repo", is_bare: false, common_dir: "/work/repo/.git" }));
    await expect(mergeVerifiedAttempt({ ...run, cwd: "/work/repo" }, "1234567890abcdef")).rejects.toThrow(/main checkout itself|no worktree to merge/);
    expect(merged()).toBe(false);
  });

  it("opens no tab for the main checkout for a merge it refuses, and opens it once for one that runs", async () => {
    fake.state.mainOpen = false;
    setup({ dirty_files: 2 });
    await expect(mergeVerifiedAttempt(run, "1234567890abcdef")).rejects.toThrow(/uncommitted file/);
    trustRepo.mockResolvedValueOnce(null);
    setup();
    await expect(mergeVerifiedAttempt(run, "1234567890abcdef")).rejects.toThrow(/not trusted/);
    expect(fake.opened).toEqual([]);
    setup();
    await mergeVerifiedAttempt(run, "1234567890abcdef");
    expect(fake.opened).toEqual(["/work/repo"]);
  });

  it("surfaces a failing merge (a conflict) as an error, leaving the resolution to the person", async () => {
    setup();
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation(async (cmd: string, args: unknown) => { if (cmd === "cmd_worktree_merge_teardown") throw new Error("CONFLICT (content): Merge conflict in src/b.ts"); return base(cmd, args); });
    await expect(mergeVerifiedAttempt(run, "1234567890abcdef")).rejects.toThrow(/Merge conflict in src\/b\.ts/);
  });

  it("throws the host's typed conflict as a MergeTeardownFailure the wave can tell from other refusals", async () => {
    setup();
    const base = invoke.getMockImplementation()!;
    const typed = { code: "conflict", message: "Merging gitpulse/fix into main conflicted in 1 file(s): src/b.ts.", paths: ["src/b.ts"], total: 1, rolled_back: true, rollback_error: null };
    invoke.mockImplementation(async (cmd: string, args: unknown) => { if (cmd === "cmd_worktree_merge_teardown") throw typed; return base(cmd, args); });
    const failure = await mergeVerifiedAttempt(run, "1234567890abcdef").catch((cause: unknown) => cause);
    expect(failure).toBeInstanceOf(MergeTeardownFailure);
    expect((failure as InstanceType<typeof MergeTeardownFailure>).failure).toEqual(typed);
  });

  it("an object that only looks like a typed error is thrown as it came", async () => {
    setup();
    const base = invoke.getMockImplementation()!;
    const lookalike = { code: "conflict", message: "no paths", paths: "src/b.ts" };
    invoke.mockImplementation(async (cmd: string, args: unknown) => { if (cmd === "cmd_worktree_merge_teardown") throw lookalike; return base(cmd, args); });
    await expect(mergeVerifiedAttempt(run, "1234567890abcdef")).rejects.toBe(lookalike);
  });

  it("reports a merge whose worktree could not be removed as merged with the cleanup pending, and keeps the tab", async () => {
    setup();
    repoState.set({ openTabs: [{ id: "t", path: WT }] });
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation(async (cmd: string, args: unknown) => cmd === "cmd_worktree_merge_teardown"
      ? { policy: null, output: teardown({ worktree_removed: false, removal_error: "The worktree could not be removed after 3 attempts: locked", branch_deleted: false, branch_error: "kept" }) }
      : base(cmd, args));
    const result = await mergeVerifiedAttempt(run, "1234567890abcdef");
    expect(result.message).toBe("Merged 1 commit from gitpulse/fix into main; cleanup pending: The worktree could not be removed after 3 attempts: locked.");
    expect(closeTab).not.toHaveBeenCalled();
  });

  it("closes the tab of a worktree the teardown removed, and says when only the branch was kept", async () => {
    setup();
    repoState.set({ openTabs: [{ id: "t", path: WT }] });
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation(async (cmd: string, args: unknown) => cmd === "cmd_worktree_merge_teardown"
      ? { policy: null, output: teardown({ branch_deleted: false, branch_error: "branch is not fully merged" }) }
      : base(cmd, args));
    const result = await mergeVerifiedAttempt(run, "1234567890abcdef");
    expect(result.message).toBe("Merged 1 commit from gitpulse/fix into main. The worktree was removed. The branch gitpulse/fix was kept: branch is not fully merged");
    expect(closeTab).toHaveBeenCalledWith("t");
  });
});
