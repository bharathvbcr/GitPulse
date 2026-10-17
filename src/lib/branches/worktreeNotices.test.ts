import { describe, expect, it } from "vitest";
import { worktreeSetupNotices } from "./types";

describe("what a worktree operation left undone", () => {
  it("says nothing when every step ran", () => {
    expect(worktreeSetupNotices({ cache_error: null, hook_error: null }, "create")).toEqual([]);
    expect(worktreeSetupNotices({ hook_error: null }, "merge")).toEqual([]);
  });

  it("names each skipped step, and that the worktree itself exists", () => {
    const notices = worktreeSetupNotices({ cache_error: "no space", hook_error: "npm ci exited 1" }, "create");
    expect(notices).toHaveLength(2);
    expect(notices[0]).toMatch(/created.*caches were not copied: no space/);
    expect(notices[1]).toMatch(/created.*post_create hook did not finish.*npm ci exited 1/);
  });

  it("after a merge whose worktree could not be removed, says it is merged and why the cleanup is pending", () => {
    const notices = worktreeSetupNotices({ worktree_removed: false, removal_error: "locked", branch_deleted: false, branch_error: "kept", hook_error: null }, "merge");
    expect(notices).toEqual(["Merged, but the worktree was not removed: locked. The merge stands; remove the worktree when it is free."]);
  });

  it("after a merge that removed the worktree but kept the branch, names the reason", () => {
    expect(worktreeSetupNotices({ worktree_removed: true, branch_deleted: false, branch_error: "not fully merged" }, "merge")).toEqual([
      "Merged and the worktree was removed, but the branch was kept: not fully merged",
    ]);
    expect(worktreeSetupNotices({ worktree_removed: true, branch_deleted: true }, "merge")).toEqual([]);
  });

  it("never reports a cleanup the creation path did not do", () => {
    expect(worktreeSetupNotices({ worktree_removed: false, removal_error: "x" }, "create")).toEqual([]);
  });

  it("reports files that could not be deleted after a removal or a merge, and never after a creation", () => {
    const cleanup = "The worktree is removed, but its files at /repo/.git/gitpulse-teardown/1-2-3 could not be deleted: busy.";
    expect(worktreeSetupNotices({ cleanup_error: cleanup }, "remove")).toEqual([cleanup]);
    expect(worktreeSetupNotices({ worktree_removed: true, branch_deleted: true, cleanup_error: cleanup }, "merge")).toEqual([cleanup]);
    expect(worktreeSetupNotices({ cleanup_error: cleanup }, "create")).toEqual([]);
    expect(worktreeSetupNotices({}, "remove")).toEqual([]);
  });

  it("reports leftovers the sweep could not clear apart from this removal's own cleanup", () => {
    const notices = worktreeSetupNotices({ sweep_problems: ["could not delete the leftover /r/.git/gitpulse-teardown/1-2-3: busy"] }, "remove");
    expect(notices).toHaveLength(1);
    expect(notices[0]).toMatch(/earlier worktree removal.*busy/);
    expect(worktreeSetupNotices({ sweep_problems: ["x"] }, "create")).toEqual([]);
  });

  it("after a teardown, says the merge happened and the cleanup did not", () => {
    expect(worktreeSetupNotices({ hook_error: "exit 4" }, "merge")).toEqual([
      "The merge and teardown finished, but the post_merge hook did not: exit 4",
    ]);
  });
});
