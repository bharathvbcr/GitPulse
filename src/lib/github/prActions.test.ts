import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  checkoutPullRequestInPlace,
  describePrAction,
  describePrWorktree,
  openPullRequestWorktree,
  type PrWorktreeOpened,
} from "./prActions";
import type { PullRequestDetail } from "./types";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

const pr: PullRequestDetail = {
  number: 7,
  title: "Add search",
  state: "OPEN",
  url: "https://github.com/acme/gitpulse/pull/7",
  is_draft: false,
  author: "ada",
  head_ref: "feat/search",
  base_ref: "main",
  head_oid: "c".repeat(40),
  body: "",
  body_truncated: false,
  additions: 1,
  deletions: 0,
  changed_files: 1,
  mergeable: "MERGEABLE",
  merge_state: "CLEAN",
  review_decision: "",
  ci_status: "success",
};

describe("describePrAction", () => {
  it("says every action is published, and where", () => {
    for (const action of [
      { kind: "create", title: "t", body: "", base: "main", draft: false } as const,
      { kind: "review", number: 7, verdict: "approve", body: "" } as const,
      { kind: "merge", number: 7, method: "squash", delete_branch: false, head_oid: pr.head_oid } as const,
    ]) {
      expect(describePrAction(action, "acme/gitpulse", { pr, headBranch: "feat/x" }).message).toContain(
        "published on GitHub (acme/gitpulse)",
      );
    }
  });

  it("names the pinned head and the branch deletion on a merge, and marks it destructive", () => {
    const confirmation = describePrAction(
      { kind: "merge", number: 7, method: "squash", delete_branch: true, head_oid: pr.head_oid },
      "acme/gitpulse",
      { pr },
    );
    expect(confirmation.destructive).toBe(true);
    expect(confirmation.message).toContain("Merge #7 Add search from feat/search into main as one squashed commit?");
    expect(confirmation.message).toContain("Only if its head is still ccccccc");
    expect(confirmation.message).toContain("deleted on GitHub and locally");
    expect(confirmation.message).toContain("cannot be undone");
  });

  it("states that creating never pushes, and quotes a review's text", () => {
    const create = describePrAction(
      { kind: "create", title: " Add search ", body: "", base: "main", draft: true },
      "acme/gitpulse",
      { headBranch: "feat/search" },
    );
    expect(create.message).toContain("Open a draft pull request from feat/search into main?");
    expect(create.message).toContain("GitPulse does not push");
    const review = describePrAction(
      { kind: "review", number: 7, verdict: "request_changes", body: "Please add tests" },
      "acme/gitpulse",
      { pr },
    );
    expect(review.message).toContain("Request changes on #7 Add search?");
    expect(review.message).toContain("Please add tests");
    expect(review.destructive).toBe(false);
  });
});

const opened: PrWorktreeOpened = {
  path: "/work/gitpulse.pr-7",
  branch: "pr/7",
  reused: false,
  head_oid: "a".repeat(40),
  cross_repository: false,
  head_ref: "feature",
  head_owner: "acme",
  created: {
    path: "/work/gitpulse.pr-7",
    cache_error: null,
    hook_error: null,
  },
  update_error: null,
};

describe("describePrWorktree", () => {
  it("says where the worktree is and nothing else when every step happened", () => {
    expect(describePrWorktree(opened)).toBe("Opened worktree pr/7 at /work/gitpulse.pr-7");
    expect(describePrWorktree({ ...opened, reused: true, created: null })).toBe(
      "Reused worktree pr/7 at /work/gitpulse.pr-7",
    );
  });

  it("says a branch that could not be updated is not the pull request head", () => {
    const text = describePrWorktree({
      ...opened,
      reused: true,
      created: null,
      update_error: "The local branch cannot fast-forward",
    });
    expect(text).toContain("pr/7 was not brought up to the pull request head");
    expect(text).toContain("cannot fast-forward");
  });

  it("surfaces cache, hook, disk and DevMap outcomes of a created worktree", () => {
    const text = describePrWorktree({
      ...opened,
      created: {
        path: opened.path,
        cache_error: "node_modules: permission denied",
        hook_error: "exit 1",
        low_disk: { cause: "below_floor", free_bytes: 1, floor_bytes: 2, cap_bytes: null, skipped: ["target"] },
        disk_unknown: true,
        devmap: { status: "failed", error: "no store" },
      },
    });
    expect(text).toContain("build caches were not copied: node_modules: permission denied");
    expect(text).toContain("post-create hook did not finish");
    expect(text).toContain("for want of disk: target");
    expect(text).toContain("Free disk space couldn't be read");
    expect(text).toContain("Code map setup failed: no store");
  });
});

describe("opening a pull request", () => {
  beforeEach(() => {
    invoke.mockReset();
  });

  it("opens in a new worktree through its own command, never the in-place checkout", async () => {
    invoke.mockResolvedValue({ policy: null, output: opened });
    const result = await openPullRequestWorktree("/repo", 7);
    expect(result.output).toEqual(opened);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("cmd_github_open_pr_worktree", {
      repoPath: "/repo",
      number: 7,
      cowCaches: true,
      runHooks: false,
    });
  });

  it("keeps in-place checkout as a separate command", async () => {
    invoke.mockResolvedValue({ policy: null, output: "Switched" });
    await checkoutPullRequestInPlace("/repo", 7);
    expect(invoke).toHaveBeenCalledWith("cmd_github_checkout_pr", { repoPath: "/repo", number: 7 });
  });

  it("rejects with the backend's refusal and records nothing", async () => {
    invoke.mockRejectedValue(new Error("Worktree path already exists: /x"));
    await expect(openPullRequestWorktree("/repo", 7)).rejects.toThrow("already exists");
  });
});
