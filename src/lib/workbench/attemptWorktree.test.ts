import { beforeEach, describe, expect, it, vi } from "vitest";
import { writable } from "svelte/store";

const invoke = vi.fn();
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
const askConfirm = vi.fn();
vi.mock("../stores/modalStore", () => ({ askConfirm: (...args: unknown[]) => askConfirm(...args) }));
type Tab = { id: string; path: string; familyRoot: string | null; currentBranch: string | null };
const repoState = writable<{ openTabs: Tab[] }>({ openTabs: [] });
const trustRepo = vi.fn(async (path: string) => path);
const closeTab = vi.fn(async (_id: string) => {});
const previewUncommitted = vi.fn(async (_path: string) => {});
let fake: ReturnType<typeof import("./attemptRepoStoreFake").fakeAttemptStore>;
vi.mock("../stores/repoStore", async () => {
  const { fakeAttemptStore } = await import("./attemptRepoStoreFake");
  fake = fakeAttemptStore((command, args) => invoke(command, args), { merge: "cmd_worktree_merge_teardown", remove: "cmd_remove_worktree", deleteBranch: "cmd_delete_branch" });
  return { repoStore: { subscribe: repoState.subscribe, trustRepo: (p: string) => trustRepo(p), closeTab: (id: string) => closeTab(id), previewUncommitted: (p: string) => previewUncommitted(p), ...fake.members } };
});
const setGlobalSurface = vi.fn();
vi.mock("../stores/interfaceStore", () => ({ interfaceStore: { setGlobalSurface: (s: string) => setGlobalSurface(s) } }));

const { MergeTeardownFailure } = await import("../branches/mergeTeardown");
const { attemptWorktreeOffer, discardAttemptWorktree, mainCheckoutOf, mergeAttemptWorktree, mergeTargetLabel, reviewAttemptChanges } = await import("./attemptWorktree");

const NOW = 1_800_000_000_000;
const WT = "/work/repo/.gitpulse/worktrees/fix-e42-wt0attem";
type RunRef = Pick<import("./client").TaskRun, "id" | "cwd" | "state" | "expires_at">;
const run = (fields: Partial<RunRef> = {}): RunRef =>
  ({ id: "wt0attempt", cwd: WT, state: "exited", expires_at: NOW / 1000 + 300, ...fields });
/** `cmd_worktree_merge_teardown`'s output for a merge that landed and cleaned up, unless `over` says otherwise. */
const teardown = (over: Record<string, unknown> = {}) => ({ merged: true, merged_branch: "gitpulse/fix-e42-wt0attem", target_branch: "main", commits_merged: 1, before_oid: "a", after_oid: "b", base_oid: null, worktree_removed: true, removal_error: null, branch_deleted: true, branch_error: null, hook_error: null, ...over });
const listing = (dirty: number | null) => [
  { path: "/work/repo", name: "repo", head: "a", branch: "main", is_bare: false, is_detached: false, is_main: true, is_locked: false, is_prunable: false, dirty_files: 0, diff_stat: null, main_divergence: null, active_routes: [] },
  { path: WT, name: "fix", head: "b", branch: "gitpulse/fix-e42-wt0attem", is_bare: false, is_detached: false, is_main: false, is_locked: false, is_prunable: false, dirty_files: dirty, diff_stat: null, main_divergence: null, active_routes: [] },
];

beforeEach(() => {
  invoke.mockReset(); askConfirm.mockReset(); trustRepo.mockClear(); closeTab.mockClear(); setGlobalSurface.mockClear(); previewUncommitted.mockClear();
  repoState.set({ openTabs: [] });
  fake.seen.length = 0; fake.state.stillNamed = () => true; fake.state.openFails = false;
});

describe("attemptWorktreeOffer", () => {
  it("offers nothing while the attempt holds its checkout, and merge/discard only for its own worktree", () => {
    expect(attemptWorktreeOffer(run({ state: "running" }), NOW)).toEqual({ review: false, ownWorktree: false });
    expect(attemptWorktreeOffer(run({ state: "prepared" }), NOW)).toEqual({ review: false, ownWorktree: false });
    expect(attemptWorktreeOffer(run(), NOW)).toEqual({ review: true, ownWorktree: true });
    // Another attempt's worktree, or a shared checkout, is never this one's to discard.
    expect(attemptWorktreeOffer(run({ id: "other123" }), NOW)).toEqual({ review: true, ownWorktree: false });
    expect(attemptWorktreeOffer(run({ cwd: "/work/repo" }), NOW)).toEqual({ review: true, ownWorktree: false });
  });
});

describe("mainCheckoutOf", () => {
  it("is the family root of an open tab, else of a fresh resolve — never the active repository", async () => {
    repoState.set({ openTabs: [{ id: "t", path: WT, familyRoot: "/work/repo", currentBranch: "gitpulse/x" }, { id: "m", path: "/work/repo", familyRoot: "/work/repo", currentBranch: "main" }] });
    expect(await mainCheckoutOf(WT)).toBe("/work/repo");
    expect(mergeTargetLabel(WT)).toBe("main");
    expect(invoke).not.toHaveBeenCalled();
    repoState.set({ openTabs: [] });
    invoke.mockResolvedValueOnce({ path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" });
    expect(await mainCheckoutOf(WT)).toBe("/work/repo");
    expect(mergeTargetLabel(WT)).toBeNull();
  });

  it("refuses the main checkout itself, and a directory whose repository cannot be told", async () => {
    invoke.mockResolvedValueOnce({ path: "/work/repo", name: "repo", is_bare: false, common_dir: "/work/repo/.git" });
    await expect(mainCheckoutOf("/work/repo")).rejects.toThrow(/main checkout, so there is no worktree/);
    invoke.mockResolvedValueOnce({ path: WT, name: "fix", is_bare: false, common_dir: null });
    await expect(mainCheckoutOf(WT)).rejects.toThrow(/could not tell which repository/);
    invoke.mockRejectedValueOnce(new Error("not a git repository"));
    await expect(mainCheckoutOf(WT)).rejects.toThrow(/could not be read as a Git worktree/);
  });
});

describe("merge and discard", () => {
  beforeEach(() => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_resolve_repo") return { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" };
      if (cmd === "cmd_list_worktrees") return listing(3);
      throw new Error(`unexpected ${cmd}`);
    });
  });

  it("merges into the main checkout's branch after asking, naming what the removal loses", async () => {
    askConfirm.mockResolvedValueOnce(true);
    invoke.mockImplementationOnce(async () => ({ path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" }))
      .mockImplementationOnce(async () => listing(3))
      .mockImplementationOnce(async () => ({ policy: null, output: teardown({ commits_merged: 2 }) }));
    const result = await mergeAttemptWorktree(run());
    expect(askConfirm.mock.calls[0][0]).toMatchObject({ title: "Merge into main?", destructive: true });
    expect(askConfirm.mock.calls[0][0].message).toContain("3 uncommitted files in it will be lost");
    // The commit the listing showed is the tip the host holds the merge to.
    expect(invoke).toHaveBeenLastCalledWith("cmd_worktree_merge_teardown", { repoPath: "/work/repo", worktreePath: WT, targetBranch: "main", squash: false, expectedTip: "b" });
    expect(result?.message).toBe("Merged 2 commits from gitpulse/fix-e42-wt0attem into main. The worktree was removed.");
  });

  it("says a merge whose cleanup did not finish is merged, with the cleanup pending, and keeps its tab", async () => {
    askConfirm.mockResolvedValueOnce(true);
    repoState.set({ openTabs: [{ id: "t", path: WT, familyRoot: "/work/repo", currentBranch: "gitpulse/x" }] });
    invoke.mockImplementationOnce(async () => listing(0))
      .mockImplementationOnce(async () => ({ policy: null, output: teardown({ worktree_removed: false, removal_error: "The worktree could not be removed after 3 attempts: locked", branch_deleted: false, branch_error: "kept" }) }));
    const result = await mergeAttemptWorktree(run());
    expect(result?.message).toBe("Merged 1 commit from gitpulse/fix-e42-wt0attem into main; cleanup pending: The worktree could not be removed after 3 attempts: locked.");
    expect(closeTab).not.toHaveBeenCalled();
  });

  it("closes the tab of a worktree the teardown removed", async () => {
    askConfirm.mockResolvedValueOnce(true);
    repoState.set({ openTabs: [{ id: "t", path: WT, familyRoot: "/work/repo", currentBranch: "gitpulse/x" }] });
    invoke.mockImplementationOnce(async () => listing(0))
      .mockImplementationOnce(async () => ({ policy: null, output: teardown({}) }));
    await mergeAttemptWorktree(run());
    expect(closeTab).toHaveBeenCalledWith("t");
  });

  it("keeps the host's typed refusal, so a conflict is told from any other failure", async () => {
    askConfirm.mockResolvedValueOnce(true);
    const typed = { code: "conflict", message: "Merging a into main conflicted in 1 file(s): f.txt.", paths: ["f.txt"], total: 1, rolled_back: true, rollback_error: null };
    repoState.set({ openTabs: [{ id: "t", path: WT, familyRoot: "/work/repo", currentBranch: "gitpulse/x" }] });
    invoke.mockImplementationOnce(async () => listing(0)).mockRejectedValueOnce(typed);
    const failure = await mergeAttemptWorktree(run()).catch((cause: unknown) => cause);
    expect(failure).toBeInstanceOf(MergeTeardownFailure);
    expect((failure as InstanceType<typeof MergeTeardownFailure>).failure).toEqual(typed);
    expect((failure as InstanceType<typeof MergeTeardownFailure>).message).toBe(typed.message);
  });

  it("does nothing when the reader declines", async () => {
    askConfirm.mockResolvedValue(false);
    expect(await mergeAttemptWorktree(run())).toBeNull();
    expect(await discardAttemptWorktree(run())).toBeNull();
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("cmd_worktree_merge_teardown");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("cmd_remove_worktree");
  });

  it("forces a removal only for changes it counted and named, and never forces the branch", async () => {
    askConfirm.mockResolvedValue(true);
    const calls: [string, unknown][] = [];
    let listings = 0;
    invoke.mockImplementation(async (cmd: string, args: unknown) => {
      calls.push([cmd, args]);
      if (cmd === "cmd_resolve_repo") return { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" };
      if (cmd === "cmd_list_worktrees") return listing(++listings === 1 ? 3 : null);
      if (cmd === "cmd_delete_branch") throw new Error("the branch is not fully merged");
      return null;
    });
    const dirty = await discardAttemptWorktree(run());
    // The dirty count is all these actions read: the listing asks for it and
    // nothing deeper (no diff stats, divergence or routes per worktree).
    expect(calls.find(([c]) => c === "cmd_list_worktrees")?.[1]).toEqual({ repoPath: "/work/repo", depth: "dirty" });
    expect(calls.find(([c]) => c === "cmd_remove_worktree")?.[1]).toEqual({ repoPath: "/work/repo", targetPath: WT, force: true });
    expect(calls.find(([c]) => c === "cmd_delete_branch")?.[1]).toEqual({ repoPath: "/work/repo", branchName: "gitpulse/fix-e42-wt0attem", force: false });
    expect(dirty?.message).toMatch(/was kept: the branch is not fully merged/);
    // Unscanned: never forced.
    calls.length = 0;
    await discardAttemptWorktree(run());
    expect(askConfirm.mock.calls.at(-1)?.[0].message).toContain("could not be counted, so only a clean removal is tried");
    expect(calls.filter(([c]) => c === "cmd_remove_worktree").at(-1)?.[1]).toMatchObject({ force: false });
  });

  it("captures the main checkout as the target before it asks, and sends every mutation to it", async () => {
    const order: string[] = [];
    const target = { tabId: "tab:/work/repo", path: "/work/repo" };
    const seenAtAsk = () => fake.seen.length;
    askConfirm.mockImplementation(async () => { order.push(`ask:${seenAtAsk()}`); return true; });
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_resolve_repo") return { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" };
      if (cmd === "cmd_list_worktrees") return listing(0);
      return null;
    });
    await discardAttemptWorktree(run());
    // Nothing had been sent when the reader was asked; the target was already held.
    expect(order).toEqual(["ask:0"]);
    expect(fake.seen).toEqual([{ command: "cmd_remove_worktree", target }, { command: "cmd_delete_branch", target }]);
  });

  it("refuses a discard whose repository changed while the reader decided, and sends nothing", async () => {
    askConfirm.mockImplementation(async () => { fake.state.stillNamed = () => false; return true; });
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_resolve_repo") return { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" };
      if (cmd === "cmd_list_worktrees") return listing(0);
      throw new Error(`unexpected ${cmd}`);
    });
    await expect(discardAttemptWorktree(run())).rejects.toThrow(/repository changed/);
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("cmd_remove_worktree");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("cmd_delete_branch");
  });

  it("says the branch was kept when the repository changed after the removal, and does not delete it", async () => {
    askConfirm.mockResolvedValue(true);
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_resolve_repo") return { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" };
      if (cmd === "cmd_list_worktrees") return listing(0);
      if (cmd === "cmd_remove_worktree") { fake.state.stillNamed = () => false; return { policy: null, output: {} }; }
      throw new Error(`unexpected ${cmd}`);
    });
    const result = await discardAttemptWorktree(run());
    expect(result?.message).toMatch(/The worktree was removed\..*was kept: The repository changed/);
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("cmd_delete_branch");
  });

  it("refuses a merge whose repository changed while the reader decided, and sends nothing", async () => {
    askConfirm.mockImplementation(async () => { fake.state.stillNamed = () => false; return true; });
    await expect(mergeAttemptWorktree(run())).rejects.toThrow(/repository changed/);
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("cmd_worktree_merge_teardown");
  });

  it("does not ask or send when the main checkout cannot be opened to hold the target", async () => {
    fake.state.openFails = true;
    await expect(discardAttemptWorktree(run())).rejects.toThrow(/could not be opened/);
    await expect(mergeAttemptWorktree(run())).rejects.toThrow(/could not be opened/);
    expect(askConfirm).not.toHaveBeenCalled();
  });

  it("review opens the checkout on its changes, in front", async () => {
    await reviewAttemptChanges(run());
    expect(previewUncommitted).toHaveBeenCalledWith(WT);
    expect(setGlobalSurface).toHaveBeenCalledWith("repository");
  });
});

