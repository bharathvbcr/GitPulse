import { describe, expect, it, vi } from "vitest";
import { loadWork } from "./load";

// Adversarial: the Work view must treat a structured prs_error as a degraded
// github source on its own, the way it already treats runs_error. Today it
// only degrades because the Rust assembler happens to ALSO push a warning.

function fakeInvoke(answers: Record<string, unknown>) {
  return vi.fn(async (cmd: string) => {
    if (!(cmd in answers)) throw new Error(`unexpected command ${cmd}`);
    return answers[cmd];
  });
}

const GITHUB = {
  available: true,
  cli_present: true,
  host: "github.com",
  owner: "o",
  repo: "r",
  html_url: "",
  pull_requests: [],
  prs_truncated: false,
  prs_error: null as string | null,
  issues: [],
  issues_truncated: false,
  issues_error: null,
  workflow_runs: [],
  runs_error: null,
  runs_truncated: false,
  releases: [],
  releases_truncated: false,
  releases_error: null,
  error: null,
  warnings: [] as string[],
};

function answers(github: unknown) {
  return {
    cmd_task_view: { available: true, store_path: "", error: "", leases: [] },
    cmd_list_worktrees: [],
    cmd_github_context: github,
    cmd_ledger_tail: [],
    cmd_grants_view: { available: true, path: "", grants: [], error: "" },
    cmd_worktree_task: null,
    cmd_repo_operation: null,
    cmd_task_scopes: [],
  };
}

describe("Work view reads prs_error", () => {
  it("degrades the github source when only prs_error is set (no warning to lean on)", async () => {
    const p = await loadWork("/repo", {
      invoke: fakeInvoke(answers({ ...GITHUB, prs_error: "gh pr list: HTTP 403" })) as never,
    });
    expect(p.sources.github.ok).toBe(false);
    expect(p.sources.github.detail).toContain("403");
  });
});
