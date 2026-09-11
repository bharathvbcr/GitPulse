import { describe, expect, it } from "vitest";
import { validateWorkResponse } from "./contracts";

describe("validateWorkResponse", () => {
  it("accepts a valid cmd_list_worktrees payload", () => {
    const payload = [{ path: "/tmp/repo", name: "main", branch: "main", is_bare: false }];
    expect(() => validateWorkResponse("cmd_list_worktrees", payload)).not.toThrow();
  });

  it("accepts a valid cmd_task_scope payload", () => {
    expect(() => validateWorkResponse("cmd_task_scope", { title: "Task name" })).not.toThrow();
  });

  it("accepts null task scope payload when response is absent", () => {
    expect(() => validateWorkResponse("cmd_task_scope", null)).not.toThrow();
  });

  it("accepts cmd_repo_operation with known kinds and numeric step counters", () => {
    expect(() =>
      validateWorkResponse("cmd_repo_operation", {
        kind: "Rebase",
        conflicted_total: 2,
        current_step: 1,
        total_steps: 3,
      }),
    ).not.toThrow();
  });

  it("accepts cmd_collision_risk payloads with itemized worktrees", () => {
    expect(() =>
      validateWorkResponse("cmd_collision_risk", {
        ok: true,
        truncated: false,
        error: "none",
        overlapping_files: 3,
        worktrees_involved: 2,
        scanned_worktrees: 2,
        unscanned_worktrees: 0,
        failed_worktrees: 0,
        items: [{ path: "src/lib", worktrees: [{ path: "wt-a", agent_kind: "runner" }] }],
      }),
    ).not.toThrow();
  });

  it("rejects malformed non-object payloads with command-scoped error", () => {
    expect(() => validateWorkResponse("cmd_task_scope", "not-an-object")).toThrow(
      "cmd_task_scope: invalid response",
    );
  });

  it("rejects cmd_repo_operation unknown operation kinds", () => {
    expect(() =>
      validateWorkResponse("cmd_repo_operation", { kind: "Unknown", conflicted_total: 0 }),
    ).toThrow("cmd_repo_operation: invalid response");
  });

  it("rejects an oversized list by enforcing response ceilings", () => {
    const items = Array.from({ length: 10_001 }, (_, index) => ({
      task_id: `${index}`,
      owner: "owner",
      status: "queued",
    }));
    expect(() => validateWorkResponse("cmd_task_view", { available: true, leases: items })).toThrow(
      "response exceeds 10000 records",
    );
  });

  it("maps all violations to a single command-scoped exception", () => {
    expect(() =>
      validateWorkResponse("cmd_github_context", {
        available: true,
        pull_requests: [{ title: "x", head_ref: 1, url: "x", ci_status: "queued", review_decision: "approve", number: 1, is_draft: false }],
      }),
    ).toThrow("cmd_github_context: invalid response");
  });
});
