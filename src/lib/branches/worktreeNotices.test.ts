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

  it("after a teardown, says the merge happened and the cleanup did not", () => {
    expect(worktreeSetupNotices({ hook_error: "exit 4" }, "merge")).toEqual([
      "The merge and teardown finished, but the post_merge hook did not: exit 4",
    ]);
  });
});
