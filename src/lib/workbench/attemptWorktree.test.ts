import { beforeEach, describe, expect, it, vi } from "vitest";
import { writable } from "svelte/store";

const invoke = vi.fn();
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
const askConfirm = vi.fn();
const askChoice = vi.fn();
vi.mock("../stores/modalStore", () => ({ askConfirm: (...args: unknown[]) => askConfirm(...args), askChoice: (...args: unknown[]) => askChoice(...args) }));
type Tab = { id: string; path: string; familyRoot: string | null; currentBranch: string | null };
/** The slice of the active session the range review reads back from the store. */
type Selection = { currentPath?: string | null; isLoading?: boolean; error?: string | null; selectionKind?: string; selectedFilePath?: string | null; selectedDiffTruncated?: boolean; selectedDiffTruncationReason?: string | null; selectedDiffError?: string | null };
const repoState = writable<{ openTabs: Tab[] } & Selection>({ openTabs: [] });
const trustRepo = vi.fn(async (path: string) => path);
const closeTab = vi.fn(async (_id: string) => {});
const previewUncommitted = vi.fn(async (_path: string) => {});
/** What the store was asked to do to the repository view, in order. */
const viewLog: string[] = [];
/** What a range selection leaves in the active session; a test replaces it to model a diff that is cut, failed or overtaken. */
let rangeAnswer: (from: string, to: string) => Selection = (from, to) => ({ selectionKind: "range", selectedFilePath: `${from}...${to}`, selectedDiffTruncated: false, selectedDiffTruncationReason: null, selectedDiffError: null });
const openRepo = vi.fn(async (path: string, extras?: { onReady?: (path: string) => void }) => {
  viewLog.push(`open:${path}`);
  repoState.update((state) => ({ ...state, currentPath: path, isLoading: false, error: null }));
  extras?.onReady?.(path);
  return true;
});
const selectRangeDiff = vi.fn(async (from: string, to: string) => {
  viewLog.push(`range:${from}...${to}`);
  repoState.update((state) => ({ ...state, ...rangeAnswer(from, to) }));
});
let fake: ReturnType<typeof import("./attemptRepoStoreFake").fakeAttemptStore>;
vi.mock("../stores/repoStore", async () => {
  const { fakeAttemptStore } = await import("./attemptRepoStoreFake");
  fake = fakeAttemptStore((command, args) => invoke(command, args), { merge: "cmd_worktree_merge_teardown", remove: "cmd_remove_worktree", deleteBranch: "cmd_delete_branch" });
  return { repoStore: { subscribe: repoState.subscribe, trustRepo: (p: string) => trustRepo(p), closeTab: (id: string) => closeTab(id), previewUncommitted: (p: string) => { viewLog.push(`uncommitted:${p}`); return previewUncommitted(p); }, openRepo: (...args: Parameters<typeof openRepo>) => openRepo(...args), selectRangeDiff: (from: string, to: string) => selectRangeDiff(from, to), ...fake.members } };
});
const setGlobalSurface = vi.fn();
vi.mock("../stores/interfaceStore", () => ({ interfaceStore: { setGlobalSurface: (s: string) => setGlobalSurface(s) } }));

const { MergeTeardownFailure } = await import("../branches/mergeTeardown");
const { toastStore } = await import("../stores/toastStore");
const { get } = await import("svelte/store");
const { attemptDirtyFiles, attemptWorktreeOffer, discardAttemptWorktree, mainCheckoutOf, mergeAttemptWorktree, mergeTargetLabel, openAttemptDiff, reviewAttemptChanges, reviewAttemptUncommitted } = await import("./attemptWorktree");

const NOW = 1_800_000_000_000;
const WT = "/work/repo/.gitpulse/worktrees/fix-e42-wt0attem";
type RunRef = Pick<import("./client").TaskRun, "id" | "cwd" | "state" | "expires_at">;
const run = (fields: Partial<RunRef> = {}): RunRef =>
  ({ id: "wt0attempt", cwd: WT, state: "exited", expires_at: NOW / 1000 + 300, ...fields });
/** `cmd_worktree_merge_teardown`'s output for a merge that landed and cleaned up, unless `over` says otherwise. */
const teardown = (over: Record<string, unknown> = {}) => ({ merged: true, merged_branch: "gitpulse/fix-e42-wt0attem", target_branch: "main", commits_merged: 1, before_oid: "a", after_oid: "b", base_oid: null, worktree_removed: true, removal_error: null, branch_deleted: true, branch_error: null, hook_error: null, ...over });
const listing = (dirty: number | null, branch = "gitpulse/fix-e42-wt0attem") => [
  { path: "/work/repo", name: "repo", head: "a", branch: "main", is_bare: false, is_detached: false, is_main: true, is_locked: false, is_prunable: false, dirty_files: 0, diff_stat: null, main_divergence: null, active_routes: [] },
  { path: WT, name: "fix", head: "b", branch, is_bare: false, is_detached: false, is_main: false, is_locked: false, is_prunable: false, dirty_files: dirty, diff_stat: null, main_divergence: null, active_routes: [] },
];

beforeEach(() => {
  invoke.mockReset(); askConfirm.mockReset(); askChoice.mockReset(); trustRepo.mockClear(); closeTab.mockClear(); setGlobalSurface.mockClear(); previewUncommitted.mockClear();
  openRepo.mockClear(); selectRangeDiff.mockClear(); viewLog.length = 0;
  rangeAnswer = (from, to) => ({ selectionKind: "range", selectedFilePath: `${from}...${to}`, selectedDiffTruncated: false, selectedDiffTruncationReason: null, selectedDiffError: null });
  toastStore.clear();
  repoState.set({ openTabs: [] });
  fake.seen.length = 0; fake.opened.length = 0; fake.state.stillNamed = () => true; fake.state.openFails = false; fake.state.mainOpen = true;
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

  it("squashes an attempt's branch into one commit by default, after asking, naming what the removal loses", async () => {
    askChoice.mockResolvedValueOnce("confirm");
    invoke.mockImplementationOnce(async () => ({ path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" }))
      .mockImplementationOnce(async () => listing(3))
      .mockImplementationOnce(async () => ({ policy: null, output: teardown({ commits_merged: 2 }) }));
    const result = await mergeAttemptWorktree(run());
    expect(askChoice.mock.calls[0][0]).toMatchObject({ title: "Merge into main?", destructive: true, confirmLabel: "Squash into one commit", alternateLabel: "Merge into main" });
    expect(askChoice.mock.calls[0][0].message).toContain("3 uncommitted files in it will be lost");
    expect(askChoice.mock.calls[0][0].message).toContain("single new commit");
    expect(askConfirm).not.toHaveBeenCalled();
    // The commit the listing showed is the tip the host holds the merge to.
    expect(invoke).toHaveBeenLastCalledWith("cmd_worktree_merge_teardown", { repoPath: "/work/repo", worktreePath: WT, targetBranch: "main", squash: true, expectedTip: "b" });
    expect(result?.message).toBe("Squashed 2 commits from gitpulse/fix-e42-wt0attem into main as one commit. The worktree was removed.");
  });

  it("holds the landing to the commit the review approved, not to whatever the listing shows", async () => {
    askChoice.mockResolvedValueOnce("confirm");
    invoke.mockImplementationOnce(async () => ({ path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" }))
      .mockImplementationOnce(async () => listing(0))
      .mockImplementationOnce(async () => ({ policy: null, output: teardown() }));
    // The listing's head is "b": the same commit the review approved.
    await mergeAttemptWorktree(run(), { expectedTip: "b" });
    expect(invoke).toHaveBeenLastCalledWith("cmd_worktree_merge_teardown", expect.objectContaining({ expectedTip: "b" }));
  });

  it("refuses, before asking, to land a worktree that moved after the review approved it", async () => {
    invoke.mockImplementationOnce(async () => ({ path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" }))
      .mockImplementationOnce(async () => listing(0));
    await expect(mergeAttemptWorktree(run(), { expectedTip: "approved1234567" })).rejects.toThrow(/moved to b after approved1234 was approved/);
    expect(askChoice).not.toHaveBeenCalled();
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("cmd_worktree_merge_teardown");
  });

  it("merges the commits as they are when the reader chooses the other way", async () => {
    askChoice.mockResolvedValueOnce("alternate");
    invoke.mockImplementationOnce(async () => ({ path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" }))
      .mockImplementationOnce(async () => listing(0))
      .mockImplementationOnce(async () => ({ policy: null, output: teardown({ commits_merged: 2 }) }));
    const result = await mergeAttemptWorktree(run());
    expect(invoke).toHaveBeenLastCalledWith("cmd_worktree_merge_teardown", { repoPath: "/work/repo", worktreePath: WT, targetBranch: "main", squash: false, expectedTip: "b" });
    expect(result?.message).toBe("Merged 2 commits from gitpulse/fix-e42-wt0attem into main. The worktree was removed.");
  });

  it("offers the squash but does not default to it for a branch that is not a gitpulse attempt branch", async () => {
    askChoice.mockResolvedValueOnce("confirm");
    invoke.mockImplementationOnce(async () => ({ path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" }))
      .mockImplementationOnce(async () => listing(0, "feature/mine"))
      .mockImplementationOnce(async () => ({ policy: null, output: teardown({ merged_branch: "feature/mine" }) }));
    await mergeAttemptWorktree(run());
    expect(askChoice.mock.calls[0][0]).toMatchObject({ confirmLabel: "Merge into main", alternateLabel: "Squash into one commit" });
    expect(invoke).toHaveBeenLastCalledWith("cmd_worktree_merge_teardown", expect.objectContaining({ squash: false }));
  });

  it("says a merge whose cleanup did not finish is merged, with the cleanup pending, and keeps its tab", async () => {
    askChoice.mockResolvedValueOnce("alternate");
    repoState.set({ openTabs: [{ id: "t", path: WT, familyRoot: "/work/repo", currentBranch: "gitpulse/x" }] });
    invoke.mockImplementationOnce(async () => listing(0))
      .mockImplementationOnce(async () => ({ policy: null, output: teardown({ worktree_removed: false, removal_error: "The worktree could not be removed after 3 attempts: locked", branch_deleted: false, branch_error: "kept" }) }));
    const result = await mergeAttemptWorktree(run());
    expect(result?.message).toBe("Merged 1 commit from gitpulse/fix-e42-wt0attem into main; cleanup pending: The worktree could not be removed after 3 attempts: locked.");
    expect(closeTab).not.toHaveBeenCalled();
  });

  it("closes the tab of a worktree the teardown removed", async () => {
    askChoice.mockResolvedValueOnce("confirm");
    repoState.set({ openTabs: [{ id: "t", path: WT, familyRoot: "/work/repo", currentBranch: "gitpulse/x" }] });
    invoke.mockImplementationOnce(async () => listing(0))
      .mockImplementationOnce(async () => ({ policy: null, output: teardown({}) }));
    await mergeAttemptWorktree(run());
    expect(closeTab).toHaveBeenCalledWith("t");
  });

  it("keeps the host's typed refusal, so a conflict is told from any other failure", async () => {
    askChoice.mockResolvedValueOnce("confirm");
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
    askChoice.mockResolvedValue(null);
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
    askChoice.mockImplementation(async () => { fake.state.stillNamed = () => false; return "confirm"; });
    await expect(mergeAttemptWorktree(run())).rejects.toThrow(/repository changed/);
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("cmd_worktree_merge_teardown");
  });

  it("sends nothing when the main checkout cannot be opened to hold the target, once the reader has approved", async () => {
    // The tab is opened only after approval (a declined confirm must leave none behind),
    // so an open that fails is said after the ask, with nothing sent.
    fake.state.mainOpen = false;
    fake.state.openFails = true;
    askConfirm.mockResolvedValue(true);
    askChoice.mockResolvedValue("confirm");
    await expect(discardAttemptWorktree(run())).rejects.toThrow(/could not be opened/);
    await expect(mergeAttemptWorktree(run())).rejects.toThrow(/could not be opened/);
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("cmd_remove_worktree");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("cmd_worktree_merge_teardown");
    expect(fake.seen).toEqual([]);
  });

  it("opens no tab for the main checkout when the reader declines, or when the work is refused before it would run", async () => {
    fake.state.mainOpen = false;
    askConfirm.mockResolvedValue(false);
    askChoice.mockResolvedValue(null);
    expect(await mergeAttemptWorktree(run())).toBeNull();
    expect(await discardAttemptWorktree(run())).toBeNull();
    trustRepo.mockResolvedValueOnce(null as never);
    askConfirm.mockResolvedValue(true);
    expect(await discardAttemptWorktree(run())).toBeNull();
    expect(fake.opened).toEqual([]);
    expect(fake.state.mainOpen).toBe(false);
  });

  it("opens the main checkout once, after approval and before it sends, when no tab holds it", async () => {
    fake.state.mainOpen = false;
    askConfirm.mockResolvedValue(true);
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_resolve_repo") return { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" };
      if (cmd === "cmd_list_worktrees") return listing(0);
      return { policy: null, output: {} };
    });
    await discardAttemptWorktree(run());
    expect(fake.opened).toEqual(["/work/repo"]);
    expect(fake.seen.map((entry) => entry.command)).toEqual(["cmd_remove_worktree", "cmd_delete_branch"]);
  });

  it("holds a tab that was already open before the ask, so closing it while the reader decides refuses the run", async () => {
    askConfirm.mockImplementation(async () => { fake.state.stillNamed = () => false; return true; });
    await expect(discardAttemptWorktree(run())).rejects.toThrow(/repository changed/);
    expect(fake.opened).toEqual([]);
    expect(fake.seen).toEqual([{ command: "cmd_remove_worktree", target: { tabId: "tab:/work/repo", path: "/work/repo" } }]);
  });

  it("review of an attempt that committed nothing opens the checkout on its uncommitted files, in front", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_attempt_review") return reviewRecord({ head_oid: BASE });
      throw new Error(`unexpected ${cmd}`);
    });
    await reviewAttemptChanges(run());
    expect(previewUncommitted).toHaveBeenCalledWith(WT);
    expect(selectRangeDiff).not.toHaveBeenCalled();
    expect(setGlobalSurface).toHaveBeenCalledWith("repository");
  });
});

const BASE = "a".repeat(40);
const HEAD = "b".repeat(40);
const reviewRecord = (over: Record<string, unknown> = {}) => ({ run_id: "wt0attempt", branch: "gitpulse/fix-e42-wt0attem", base_oid: BASE, head_oid: HEAD, files_changed: 2, ended: true, review: { status: "unreviewed" }, gate_on: true, gate_error: null, ...over });
const freshness = (over: Record<string, unknown> = {}) => ({ commit_sha: BASE, landed_on_base: true, on_base: true, base_oid: HEAD, merge_base: BASE, ahead: 0, behind: 2, base_relation_reason: "", ...over });
/** The host as an attempt that committed two files shows it: its review record, and base reachable from head unless `over` says otherwise. */
function committedAttempt(over: { review?: Record<string, unknown>; freshness?: unknown } = {}) {
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === "cmd_attempt_review") return reviewRecord(over.review);
    if (cmd === "cmd_provenance_freshness") {
      if (over.freshness instanceof Error) throw over.freshness;
      return over.freshness ?? freshness();
    }
    if (cmd === "cmd_resolve_repo") return { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" };
    if (cmd === "cmd_list_worktrees") return listing(0);
    throw new Error(`unexpected ${cmd}`);
  });
}
const warnings = () => get(toastStore).filter((toast) => toast.kind === "warning").map((toast) => toast.message);

describe("reviewing what an attempt committed", () => {
  it("opens the committed range in the attempt's own checkout, selecting only once that checkout is ready", async () => {
    committedAttempt();
    // Another repository is in front: a selection made now would diff that one.
    repoState.set({ openTabs: [], currentPath: "/work/repo", isLoading: false, error: null });
    await reviewAttemptChanges(run());
    expect(viewLog).toEqual([`open:${WT}`, `range:${BASE}...${HEAD}`]);
    expect(previewUncommitted).not.toHaveBeenCalled();
    expect(setGlobalSurface).toHaveBeenCalledWith("repository");
  });

  it("selects directly when the attempt's checkout is already the ready tab in front", async () => {
    committedAttempt();
    repoState.set({ openTabs: [], currentPath: WT, isLoading: false, error: null });
    await reviewAttemptChanges(run());
    expect(viewLog).toEqual([`range:${BASE}...${HEAD}`]);
  });

  it("reopens a checkout whose tab is still loading or failed, rather than selecting into it", async () => {
    committedAttempt();
    repoState.set({ openTabs: [], currentPath: WT, isLoading: true, error: null });
    await reviewAttemptChanges(run());
    expect(viewLog[0]).toBe(`open:${WT}`);
  });

  it("asks whether base is an ancestor of head, in the attempt's checkout, with no git spawned here", async () => {
    committedAttempt();
    const opened = await openAttemptDiff(run(), reviewRecord() as never);
    expect(invoke).toHaveBeenCalledWith("cmd_provenance_freshness", { repoPath: WT, commitSha: BASE, baseBranch: HEAD });
    expect(opened).toMatchObject({ shown: "range", notices: [], truncated: false, from: BASE, to: HEAD });
    expect(warnings()).toEqual([]);
  });

  it("says plainly that the three-dot diff is not the approved range when base is not an ancestor of head", async () => {
    committedAttempt({ freshness: freshness({ landed_on_base: false, on_base: false, merge_base: "c".repeat(40), ahead: 3 }) });
    const opened = await openAttemptDiff(run(), reviewRecord() as never);
    expect(opened.shown).toBe("range");
    expect(opened.notices).toHaveLength(1);
    expect(opened.notices[0]).toContain(`${BASE.slice(0, 12)} is not an ancestor of ${HEAD.slice(0, 12)}`);
    expect(opened.notices[0]).toContain("not the range the review covers");
    // Said where the reader lands, not only in the panel they left.
    expect(warnings()).toEqual(opened.notices);
  });

  it("does not call an unmeasured ancestry a failed one", async () => {
    committedAttempt({ freshness: freshness({ landed_on_base: null, on_base: null, ahead: null, behind: null, base_relation_reason: "not measured: this is a shallow clone" }) });
    const opened = await openAttemptDiff(run(), reviewRecord() as never);
    expect(opened.notices[0]).toContain("could not be told");
    expect(opened.notices[0]).toContain("this is a shallow clone");
    expect(opened.notices[0]).not.toContain("is not an ancestor");
    committedAttempt({ freshness: new Error("host gone") });
    const failed = await openAttemptDiff(run(), reviewRecord() as never);
    expect(failed.notices[0]).toContain("could not be told");
    expect(failed.notices[0]).toContain("host gone");
  });

  it("surfaces a truncated range diff, saying the review covers the whole range", async () => {
    committedAttempt();
    rangeAnswer = (from, to) => ({ selectionKind: "range", selectedFilePath: `${from}...${to}`, selectedDiffTruncated: true, selectedDiffTruncationReason: "larger than GitPulse reads in one go", selectedDiffError: null });
    const opened = await openAttemptDiff(run(), reviewRecord() as never);
    expect(opened.truncated).toBe(true);
    expect(opened.notices[0]).toContain("Only part of this diff is shown");
    expect(opened.notices[0]).toContain("larger than GitPulse reads in one go");
    expect(opened.notices[0]).toContain("all 2 files");
    expect(warnings()).toContain(opened.notices[0]);
  });

  it("says when the diff could not be read, or the view moved on before it was selected", async () => {
    committedAttempt();
    rangeAnswer = (from, to) => ({ selectionKind: "range", selectedFilePath: `${from}...${to}`, selectedDiffTruncated: false, selectedDiffError: "bad revision" });
    expect((await openAttemptDiff(run(), reviewRecord() as never)).notices.join(" ")).toContain("could not be read: bad revision");
    // Something else took the selection before this one's answer: the view must not be reported as showing the range.
    rangeAnswer = () => ({ selectionKind: "file", selectedFilePath: "other.txt", selectedDiffTruncated: false, selectedDiffError: null });
    const overtaken = await openAttemptDiff(run(), reviewRecord() as never);
    expect(overtaken.notices.join(" ")).toContain("moved on");
  });

  it("falls back to the uncommitted files, saying why, when the review cannot be read", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_attempt_review") throw new Error("store busy");
      throw new Error(`unexpected ${cmd}`);
    });
    await reviewAttemptChanges(run());
    expect(previewUncommitted).toHaveBeenCalledWith(WT);
    expect(selectRangeDiff).not.toHaveBeenCalled();
    expect(warnings().join(" ")).toContain("commits could not be read");
    expect(warnings().join(" ")).toContain("store busy");
  });

  it("keeps the uncommitted files as its own action, and counts them from the listing", async () => {
    committedAttempt();
    await reviewAttemptUncommitted(run());
    expect(previewUncommitted).toHaveBeenCalledWith(WT);
    expect(setGlobalSurface).toHaveBeenCalledWith("repository");
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_resolve_repo") return { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" };
      if (cmd === "cmd_list_worktrees") return listing(4);
      throw new Error(`unexpected ${cmd}`);
    });
    expect(await attemptDirtyFiles(run())).toBe(4);
    invoke.mockImplementation(async (cmd: string) => (cmd === "cmd_resolve_repo" ? { path: WT, name: "fix", is_bare: false, common_dir: "/work/repo/.git" } : listing(null)));
    expect(await attemptDirtyFiles(run())).toBeNull();
  });
});

