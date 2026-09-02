import { describe, expect, it, vi } from "vitest";
import { loadWork, MAX_BINDING_LOOKUPS } from "./load";
import { UNBOUND_ROW_ID } from "./projection";

/** A fake IPC surface: every command answers from `answers`, or throws. */
function fakeInvoke(answers: Record<string, unknown | (() => unknown)>) {
  return vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    if (!(cmd in answers)) throw new Error(`unexpected command ${cmd}`);
    const answer = answers[cmd];
    const value = typeof answer === "function" ? (answer as (a: unknown) => unknown)(args) : answer;
    if (value instanceof Error) throw value;
    return value;
  });
}

const OK_TASKS = {
  available: true,
  store_path: "/repo/.devcouncil/state.sqlite",
  error: "",
  leases: [
    {
      task_id: "TASK-1",
      owner: "agent",
      agent: "claude",
      branch: "feature/x",
      status: "in_progress",
      created_at: "2026-09-01T12:00:00Z",
      expires_at: null,
    },
  ],
};

const OK_WORKTREES = [
  {
    path: "/repo",
    name: "repo",
    head: "a".repeat(40),
    branch: "main",
    is_bare: false,
    is_detached: false,
    is_main: true,
    is_locked: false,
    is_prunable: false,
    dirty_files: 0,
  },
];

const OK_GITHUB = {
  available: true,
  cli_present: true,
  host: "github.com",
  owner: "o",
  repo: "r",
  html_url: "",
  pull_requests: [],
  prs_truncated: false,
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
  warnings: [],
};

const OK_GRANTS = { available: true, path: "/repo/.devcouncil/harness-grants.json", grants: [], error: "" };

function baseAnswers(over: Record<string, unknown> = {}) {
  return {
    cmd_task_view: OK_TASKS,
    cmd_list_worktrees: OK_WORKTREES,
    cmd_github_context: OK_GITHUB,
    cmd_ledger_tail: [],
    cmd_grants_view: OK_GRANTS,
    cmd_worktree_task: null,
    cmd_task_scope: { id: "TASK-1", title: "Close the gate bypass", status: "in_progress", planned_files: [], agent_appended_files: [], forbidden_changes: [], allowed_commands: [] },
    ...over,
  };
}

describe("loadWork", () => {
  it("joins every source into rows", async () => {
    const invoke = fakeInvoke(
      baseAnswers({
        cmd_worktree_task: (args: { worktreePath: string }) =>
          args.worktreePath === "/repo" ? "TASK-1" : null,
      }),
    );
    const p = await loadWork("/repo", { invoke: invoke as never });

    expect(p.degraded).toBe(false);
    expect(p.rows.map((r) => r.taskId)).toEqual(["TASK-1"]);
    expect(p.rows[0].title).toBe("Close the gate bypass");
    expect(p.rows[0].worktrees[0].worktree.path).toBe("/repo");
  });

  it("degrades rather than blanking when one source fails", async () => {
    // A screen that vanishes on one bad source tells the reader less than one
    // that names the source.
    const invoke = fakeInvoke(baseAnswers({ cmd_github_context: new Error("gh: not found") }));
    const p = await loadWork("/repo", { invoke: invoke as never });

    expect(p.degraded).toBe(true);
    expect(p.sources.github.ok).toBe(false);
    expect(p.sources.github.detail).toContain("gh: not found");
    // The rest still assembled.
    expect(p.sources.tasks.ok).toBe(true);
    expect(p.rows.length).toBeGreaterThan(0);
  });

  it("treats an absent source as absent, not as a failure", async () => {
    const invoke = fakeInvoke(
      baseAnswers({
        cmd_task_view: { available: false, store_path: "", leases: [], error: "" },
        cmd_grants_view: { available: false, path: "", grants: [], error: "" },
      }),
    );
    const p = await loadWork("/repo", { invoke: invoke as never });

    expect(p.degraded).toBe(false);
    expect(p.sources.tasks.present).toBe(false);
    expect(p.sources.grants.present).toBe(false);
  });

  it("reports a store that exists and could not be read", async () => {
    // The distinction `available` + `error` exists to draw: no store here, and
    // a store we failed to open, are different facts.
    const invoke = fakeInvoke(
      baseAnswers({
        cmd_task_view: { available: true, store_path: "/s", leases: [], error: "database is locked" },
      }),
    );
    const p = await loadWork("/repo", { invoke: invoke as never });

    expect(p.sources.tasks.ok).toBe(false);
    expect(p.sources.tasks.present).toBe(true);
    expect(p.sources.tasks.detail).toBe("database is locked");
    expect(p.degraded).toBe(true);
  });

  it("degrades github when a section inside it failed", async () => {
    // An empty runs list because `gh` choked is not the same fact as there
    // being no runs.
    const invoke = fakeInvoke(
      baseAnswers({
        cmd_github_context: { ...OK_GITHUB, runs_error: "gh run list: HTTP 502" },
      }),
    );
    const p = await loadWork("/repo", { invoke: invoke as never });

    expect(p.sources.github.ok).toBe(false);
    expect(p.sources.github.detail).toContain("502");
  });

  it("says so when it could not resolve every worktree binding", async () => {
    // Past the cap a worktree lands in the unbound bucket, which is exactly
    // what a genuinely unbound worktree looks like.
    const many = Array.from({ length: MAX_BINDING_LOOKUPS + 3 }, (_, i) => ({
      ...OK_WORKTREES[0],
      path: `/wt/${i}`,
      name: `${i}`,
      is_main: false,
    }));
    const invoke = fakeInvoke(baseAnswers({ cmd_list_worktrees: many }));
    const p = await loadWork("/repo", { invoke: invoke as never });

    expect(p.sources.worktrees.ok).toBe(false);
    expect(p.sources.worktrees.detail).toContain(`${MAX_BINDING_LOOKUPS}`);
    expect(p.degraded).toBe(true);
  });

  it("survives a binding lookup that throws", async () => {
    const invoke = fakeInvoke(
      baseAnswers({ cmd_worktree_task: new Error("ledger is locked") }),
    );
    const p = await loadWork("/repo", { invoke: invoke as never });

    // The worktree is simply unbound; nothing else is lost.
    expect(p.rows.find((r) => r.taskId === UNBOUND_ROW_ID)!.worktrees).toHaveLength(1);
    expect(p.sources.worktrees.ok).toBe(true);
  });

  it("never rejects", async () => {
    const invoke = vi.fn().mockRejectedValue(new Error("everything is on fire"));
    const p = await loadWork("/repo", { invoke: invoke as never });

    expect(p.degraded).toBe(true);
    expect(Object.values(p.sources).every((s) => !s.ok)).toBe(true);
    expect(p.rows).toEqual([]);
  });

  it("reads the ledger and the task store in one round of calls", async () => {
    const invoke = fakeInvoke(baseAnswers());
    await loadWork("/repo", { invoke: invoke as never });

    const commands = invoke.mock.calls.map((c) => c[0]);
    expect(commands).toContain("cmd_task_view");
    expect(commands).toContain("cmd_list_worktrees");
    expect(commands).toContain("cmd_github_context");
    expect(commands).toContain("cmd_ledger_tail");
    expect(commands).toContain("cmd_grants_view");
  });
});
