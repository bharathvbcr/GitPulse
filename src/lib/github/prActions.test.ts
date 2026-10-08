import { describe, expect, it } from "vitest";
import { describePrAction } from "./prActions";
import type { PullRequestDetail } from "./types";

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
