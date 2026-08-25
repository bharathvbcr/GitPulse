import { describe, expect, it } from "vitest";
import { classifyDeleteFailure, escalateDeleteDecision } from "./deleteEscalation";
import type { BranchInfo } from "./types";

function branchInfo(overrides: Partial<BranchInfo> = {}): BranchInfo {
  return {
    name: "feat/x",
    is_current: false,
    is_remote: false,
    remote_name: null,
    tip_commit_id: "tip",
    ahead_count: 0,
    behind_count: 0,
    upstream: "origin/feat/x",
    is_default: false,
    is_gone: false,
    last_commit_timestamp: 0,
    last_author: "ada",
    last_summary: "wip",
    commits_ahead_of_base: 3,
    commits_behind_base: 0,
    additions: 0,
    deletions: 0,
    files_changed: 0,
    compared_to: "main",
    ...overrides,
  };
}

describe("classifyDeleteFailure", () => {
  it.each([
    [
      "modern lowercase unmerged wording",
      "error: the branch 'feat/x' is not fully merged.\nIf you are sure you want to delete it, run 'git branch -D feat/x'.",
      "unmerged",
    ],
    [
      "legacy capitalised unmerged wording",
      "error: The branch 'feat/x' is not fully merged.",
      "unmerged",
    ],
    [
      "default-branch guard refusal (Rust)",
      "refusing to force-delete 'main': it resolves to the repository's default branch",
      "default-branch",
    ],
    [
      "linked-worktree guard refusal (Rust)",
      "refusing to force-delete 'feat/x': it is checked out in a linked worktree",
      "worktree-checked-out",
    ],
    [
      "git's own worktree refusal on -d",
      "error: cannot delete branch 'feat/x' used by worktree at '/repo/wt'",
      "worktree-checked-out",
    ],
    ["unknown failure", "fatal: bad object HEAD", "other"],
    ["empty failure", "", "other"],
  ] as const)("maps %s", (_label, errorText, expected) => {
    expect(classifyDeleteFailure(errorText)).toBe(expected);
  });
});

describe("escalateDeleteDecision", () => {
  const branch = branchInfo();

  it.each([
    [
      "unmerged refusal escalates to a force retry",
      "error: the branch 'feat/x' is not fully merged.",
      { kind: "unmerged", canRetryForce: true },
    ],
    [
      "default-branch refusal does NOT offer a force retry",
      "refusing to force-delete 'main': it resolves to the repository's default branch",
      { kind: "default-branch", canRetryForce: false },
    ],
    [
      "linked-worktree refusal does NOT offer a force retry",
      "refusing to force-delete 'feat/x': it is checked out in a linked worktree",
      { kind: "worktree-checked-out", canRetryForce: false },
    ],
    [
      "unknown refusal does not escalate",
      "fatal: something else entirely",
      { kind: "other", canRetryForce: false },
    ],
  ] as const)("%s", (_label, errorText, expected) => {
    const decision = escalateDeleteDecision(errorText, branch);
    expect(decision.kind).toBe(expected.kind);
    expect(decision.canRetryForce).toBe(expected.canRetryForce);
  });

  it("offers no confirm copy for protected refusals and unknown failures", () => {
    expect(
      escalateDeleteDecision(
        "refusing to force-delete 'feat/x': it is checked out in a linked worktree",
        branch,
      ).message,
    ).toBeNull();
    expect(
      escalateDeleteDecision("refusing to force-delete 'main': it resolves to the repository's default branch", branch)
        .message,
    ).toBeNull();
    expect(escalateDeleteDecision("fatal: nope", branch).message).toBeNull();
  });

  it("includes the ahead count in the escalation copy", () => {
    const message = escalateDeleteDecision("error: the branch 'feat/x' is not fully merged.", branch).message ?? "";
    expect(message).toContain("3 commits not on main");
  });

  it("singularises one commit", () => {
    const message =
      escalateDeleteDecision("error: the branch 'feat/x' is not fully merged.", branchInfo({ commits_ahead_of_base: 1 }))
        .message ?? "";
    expect(message).toContain("1 commit not on main");
    expect(message).not.toContain("1 commits");
  });

  it("includes gone-upstream wording when the upstream is gone", () => {
    const message =
      escalateDeleteDecision(
        "error: the branch 'feat/x' is not fully merged.",
        branchInfo({ is_gone: true }),
      ).message ?? "";
    expect(message).toContain("upstream is gone");
    expect(message).toContain("exist nowhere else");
  });

  it("omits gone-upstream wording when the upstream still exists", () => {
    const message = escalateDeleteDecision("error: the branch 'feat/x' is not fully merged.", branch).message ?? "";
    expect(message).not.toContain("upstream is gone");
  });

  it("falls back to a generic base label when compared_to is missing", () => {
    const message =
      escalateDeleteDecision(
        "error: the branch 'feat/x' is not fully merged.",
        branchInfo({ compared_to: null }),
      ).message ?? "";
    expect(message).toContain("not on the base branch");
  });

  it("carries a freshness caveat so a stale menu snapshot cannot pose as live data", () => {
    const message = escalateDeleteDecision("error: the branch 'feat/x' is not fully merged.", branch).message ?? "";
    expect(message.toLowerCase()).toContain("last refresh");
  });
});
