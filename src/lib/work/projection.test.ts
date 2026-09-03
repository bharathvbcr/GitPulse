import { describe, expect, it } from "vitest";
import {
  ALL_STATUSES,
  degradedSummary,
  emptyTally,
  insightSummary,
  noteworthyStatuses,
  openPathFor,
  parkedWorktree,
  projectWork,
  UNBOUND_ROW_ID,
  dirtyCount,
  verdictStatus,
  type WorkInputs,
  type WorkSources,
} from "./projection";
import type { LedgerEvent } from "../ledger/types";
import type { WorktreeInfo } from "../branches/types";
import type { PullRequestInfo, WorkflowRunInfo } from "../github/types";
import type { Grant } from "../grants/types";
import type { TaskLease } from "../tasks/types";

function source(over: Partial<WorkSources[keyof WorkSources]> = {}) {
  return { ok: true, present: true, detail: "", ...over };
}

function sources(over: Partial<WorkSources> = {}): WorkSources {
  return {
    tasks: source(),
    worktrees: source(),
    github: source(),
    ledger: source(),
    grants: source(),
    ...over,
  };
}

function inputs(over: Partial<WorkInputs> = {}): WorkInputs {
  return {
    leases: [],
    titles: {},
    worktrees: [],
    bindings: {},
    pullRequests: [],
    runs: [],
    events: [],
    grants: [],
    operations: {},
    sources: sources(),
    ...over,
  };
}

function worktree(path: string, branch: string | null): WorktreeInfo {
  return {
    path,
    name: path.split("/").pop() ?? path,
    head: "a".repeat(40),
    branch,
    is_bare: false,
    is_detached: branch === null,
    is_main: false,
    is_locked: false,
    is_prunable: false,
    dirty_files: 0,
  };
}

function lease(task_id: string, over: Partial<TaskLease> = {}): TaskLease {
  return {
    task_id,
    owner: "agent-1",
    agent: "claude",
    branch: null,
    status: "in_progress",
    created_at: "2026-09-01T12:00:00Z",
    expires_at: null,
    ...over,
  };
}

function pr(number: number, head_ref: string): PullRequestInfo {
  return {
    number,
    title: `PR ${number}`,
    state: "OPEN",
    head_ref,
    base_ref: "main",
    url: `https://example.test/${number}`,
    is_draft: false,
    ci_status: "success",
    created_at: "",
    updated_at: "",
    review_decision: "",
    first_review_at: "",
  };
}

function run(id: number, head_branch: string): WorkflowRunInfo {
  return {
    id,
    name: "ci",
    title: "CI",
    status: "completed",
    conclusion: "success",
    head_branch,
    url: `https://example.test/run/${id}`,
    created_at: "",
  };
}

function grant(id: string, task_id: string): Grant {
  return {
    id,
    grantor: { authority: "human", name: "bharath" },
    reason: "needed for the migration",
    scope: { rule: "scope.unplanned", target: "src/a.rs", task_id },
    issued_at: "2026-09-01T12:00:00Z",
    expires_at: "2026-09-01T13:00:00Z",
    consumed: false,
  };
}

function event(over: Partial<LedgerEvent> = {}): LedgerEvent {
  return {
    id: 1,
    ulid: "01J".padEnd(26, "0"),
    ts_utc: "2026-09-01T12:00:00Z",
    schema_version: 1,
    repo_path: "/repo",
    worktree_path: null,
    actor_kind: "agent",
    actor_id: null,
    session_id: null,
    task_id: null,
    action: "git.commit",
    object: null,
    argv_json: null,
    outcome: "ok",
    verdict_json: null,
    before_ref: null,
    after_ref: null,
    duration_ms: null,
    detail_json: null,
    ...over,
  };
}

describe("projectWork joins", () => {
  it("puts a worktree on the task the ledger bound it to, not on its branch name", () => {
    // A binding is a recorded decision; a branch name is a coincidence. Two
    // worktrees can hold the same branch, and neither of them is evidence of
    // which task the work belongs to.
    const p = projectWork(
      inputs({
        leases: [lease("TASK-1")],
        worktrees: [worktree("/wt/a", "feature/x")],
        bindings: { "/wt/a": "TASK-1" },
      }),
    );
    expect(p.rows.map((r) => r.taskId)).toEqual(["TASK-1"]);
    expect(p.rows[0].worktrees[0].worktree.path).toBe("/wt/a");
  });

  it("routes a pull request through the worktree branch that carries it", () => {
    const p = projectWork(
      inputs({
        leases: [lease("TASK-1")],
        worktrees: [worktree("/wt/a", "feature/x")],
        bindings: { "/wt/a": "TASK-1" },
        pullRequests: [pr(7, "feature/x")],
        runs: [run(90, "feature/x")],
      }),
    );
    expect(p.rows[0].pullRequests.map((x) => x.number)).toEqual([7]);
    expect(p.rows[0].runs.map((x) => x.id)).toEqual([90]);
  });

  it("shows a shared branch on every task that claims it rather than picking one", () => {
    // Arbitrarily assigning it would hide the work from one of the two tasks
    // it actually belongs to, with nothing on screen to say so.
    const p = projectWork(
      inputs({
        leases: [lease("TASK-1"), lease("TASK-2")],
        worktrees: [worktree("/wt/a", "shared"), worktree("/wt/b", "shared")],
        bindings: { "/wt/a": "TASK-1", "/wt/b": "TASK-2" },
        pullRequests: [pr(7, "shared")],
      }),
    );
    const withPr = p.rows.filter((r) => r.pullRequests.length > 0).map((r) => r.taskId);
    expect(withPr.sort()).toEqual(["TASK-1", "TASK-2"]);
  });

  it("does not guess a task for a pull request no worktree matches", () => {
    const p = projectWork(
      inputs({
        leases: [lease("TASK-1")],
        worktrees: [worktree("/wt/a", "feature/x")],
        bindings: { "/wt/a": "TASK-1" },
        pullRequests: [pr(9, "somebody-elses-branch")],
      }),
    );
    expect(p.rows.find((r) => r.taskId === "TASK-1")!.pullRequests).toEqual([]);
    expect(
      p.rows.find((r) => r.taskId === UNBOUND_ROW_ID)!.pullRequests.map((x) => x.number),
    ).toEqual([9]);
  });

  it("attributes verdicts and grants by what was recorded, never inferred", () => {
    const p = projectWork(
      inputs({
        leases: [lease("TASK-1")],
        grants: [grant("G-1", "TASK-1"), grant("G-2", "")],
        events: [
          event({ id: 1, task_id: "TASK-1", verdict_json: '{"status":"blocked"}' }),
          event({ id: 2, task_id: "TASK-1", verdict_json: '{"status":"allowed"}' }),
          event({ id: 3, task_id: null, verdict_json: '{"status":"granted"}' }),
        ],
      }),
    );
    const t1 = p.rows.find((r) => r.taskId === "TASK-1")!;
    expect(t1.verdicts.byStatus.blocked).toBe(1);
    expect(t1.verdicts.byStatus.allowed).toBe(1);
    expect(t1.verdicts.events).toBe(2);
    expect(t1.grants.map((g) => g.id)).toEqual(["G-1"]);

    const unbound = p.rows.find((r) => r.taskId === UNBOUND_ROW_ID)!;
    expect(unbound.verdicts.byStatus.granted).toBe(1);
    expect(unbound.grants.map((g) => g.id)).toEqual(["G-2"]);
  });

  it("keeps the unbound bucket last however much it holds", () => {
    const p = projectWork(
      inputs({
        leases: [lease("TASK-1")],
        worktrees: [
          worktree("/wt/a", "feature/x"),
          worktree("/wt/b", "b"),
          worktree("/wt/c", "c"),
          worktree("/wt/d", "d"),
        ],
        bindings: { "/wt/a": "TASK-1" },
      }),
    );
    expect(p.rows.at(-1)!.taskId).toBe(UNBOUND_ROW_ID);
    expect(p.rows.at(-1)!.worktrees).toHaveLength(3);
  });

  it("orders busier tasks first and breaks ties by id", () => {
    const p = projectWork(
      inputs({
        leases: [lease("TASK-B"), lease("TASK-A"), lease("TASK-C")],
        worktrees: [worktree("/wt/c", "c")],
        bindings: { "/wt/c": "TASK-C" },
      }),
    );
    expect(p.rows.map((r) => r.taskId)).toEqual(["TASK-C", "TASK-A", "TASK-B"]);
  });

  it("names a task from its scope when the lease does not", () => {
    const p = projectWork(
      inputs({ leases: [lease("TASK-1")], titles: { "TASK-1": "Close the gate bypass" } }),
    );
    expect(p.rows[0].title).toBe("Close the gate bypass");
  });
});

describe("the honesty invariant", () => {
  it("says the screen is incomplete when a source could not be read", () => {
    const p = projectWork(
      inputs({
        pullRequests: null,
        sources: sources({ github: { ok: false, present: true, detail: "gh is not installed" } }),
      }),
    );
    expect(p.degraded).toBe(true);
    expect(degradedSummary(p.sources)).toContain("github");
    expect(degradedSummary(p.sources)).toContain("gh is not installed");
  });

  it("is not degraded merely because a source is absent", () => {
    // A repository with no DevCouncil store is the ordinary case. Reporting it
    // as a failure would make the warning meaningless everywhere it matters.
    const p = projectWork(
      inputs({
        leases: [],
        sources: sources({ tasks: { ok: true, present: false, detail: "" } }),
      }),
    );
    expect(p.degraded).toBe(false);
    expect(degradedSummary(p.sources)).toBe("");
  });

  it("never counts an unreadable verdict as an allow", () => {
    // The tempting `?? "allowed"` here would turn every verdict this build
    // cannot parse into a clean pass — a check that could not be read
    // rendering as a check that ran and passed.
    const p = projectWork(
      inputs({
        events: [
          event({ id: 1, verdict_json: "{ not json" }),
          event({ id: 2, verdict_json: '{"status":"invented-status"}' }),
          event({ id: 3, verdict_json: "{}" }),
        ],
      }),
    );
    const row = p.rows[0];
    expect(row.verdicts.unparsed).toBe(3);
    expect(row.verdicts.byStatus.allowed).toBe(0);
  });

  it("distinguishes an unjudged event from an unreadable verdict", () => {
    const p = projectWork(
      inputs({ events: [event({ id: 1, verdict_json: null }), event({ id: 2, verdict_json: "x" })] }),
    );
    expect(p.rows[0].verdicts.events).toBe(2);
    expect(p.rows[0].verdicts.unparsed).toBe(1);
    expect(Object.values(p.rows[0].verdicts.byStatus).every((n) => n === 0)).toBe(true);
  });

  it("produces no rows at all from nothing, rather than an empty task", () => {
    expect(projectWork(inputs()).rows).toEqual([]);
  });
});

describe("verdictStatus", () => {
  it("is null for an unjudged row and unparsed for one it cannot read", () => {
    expect(verdictStatus(event({ verdict_json: null }))).toBeNull();
    expect(verdictStatus(event({ verdict_json: "" }))).toBeNull();
    expect(verdictStatus(event({ verdict_json: "nope" }))).toBe("unparsed");
    expect(verdictStatus(event({ verdict_json: '{"status":42}' }))).toBe("unparsed");
  });

  it("recognises every status the policy union declares", () => {
    // Derived from the union rather than hand-listed: a ninth status added to
    // PolicyStatus without a case here would silently start counting as
    // unparsed, and this fails the moment that happens.
    for (const status of ALL_STATUSES) {
      expect(verdictStatus(event({ verdict_json: JSON.stringify({ status }) }))).toBe(status);
    }
  });
});

describe("tally helpers", () => {
  it("starts every status at zero so a chip is never undefined", () => {
    const tally = emptyTally();
    for (const status of ALL_STATUSES) expect(tally.byStatus[status]).toBe(0);
  });

  it("keeps allowed out of the chips and everything else in", () => {
    const tally = emptyTally();
    tally.byStatus.allowed = 90;
    tally.byStatus.blocked = 2;
    tally.byStatus.degraded = 1;
    expect(noteworthyStatuses(tally)).toEqual([
      ["degraded", 1],
      ["blocked", 2],
    ]);
  });
});

/**
 * The repository this app is actually used in: no DevCouncil store, several
 * Claude Code worktrees, each on its own branch. Keying on task collapsed all
 * of it into one row labelled "Not bound to a task" — a screen that reported
 * nothing while four agents were mid-change.
 */
describe("worktree rows, when there is no task store", () => {
  function op(kind = "Rebase" as const, extra = {}) {
    return {
      kind,
      current_step: 2,
      total_steps: 7,
      head_ref: "feature",
      incoming_ref: null,
      conflicted_paths: ["a.ts"],
      conflicted_total: 1,
      available: ["abort" as const],
      warnings: [],
      ...extra,
    };
  }

  it("gives every worktree its own row instead of one unbound blob", () => {
    const p = projectWork(
      inputs({
        leases: null,
        worktrees: [
          worktree("/repo", "main"),
          worktree("/wt/alpha", "claude/alpha"),
          worktree("/wt/beta", "claude/beta"),
        ],
      }),
    );
    expect(p.rows).toHaveLength(3);
    expect(p.rows.every((r) => r.kind === "worktree")).toBe(true);
    expect(p.rows.map((r) => r.key).sort()).toEqual(["/repo", "/wt/alpha", "/wt/beta"]);
    // And none of them is the catch-all.
    expect(p.rows.some((r) => r.key === UNBOUND_ROW_ID)).toBe(false);
  });

  it("titles each row with its branch", () => {
    const p = projectWork(
      inputs({ leases: null, worktrees: [worktree("/wt/alpha", "claude/alpha")] }),
    );
    expect(p.rows[0].title).toBe("claude/alpha");
    // A worktree row carries no task id — claiming one would be a fiction.
    expect(p.rows[0].taskId).toBe("");
  });

  it("falls back to the directory name for a detached worktree", () => {
    // "" as a title renders as an untitled row the user cannot identify.
    const p = projectWork(inputs({ leases: null, worktrees: [worktree("/wt/detached", null)] }));
    expect(p.rows[0].title).toBe("detached");
  });

  it("attaches a parked operation to the worktree it is parked in", () => {
    const p = projectWork(
      inputs({
        leases: null,
        worktrees: [worktree("/wt/alpha", "claude/alpha"), worktree("/wt/beta", "claude/beta")],
        operations: { "/wt/beta": op() },
      }),
    );
    expect(p.rows.find((r) => r.key === "/wt/alpha")!.operation).toBeNull();
    expect(p.rows.find((r) => r.key === "/wt/beta")!.operation?.kind).toBe("Rebase");
  });

  it("puts a blocked worktree above a busier unblocked one", () => {
    // Being stuck mid-rebase is the one thing on this screen that needs a
    // person right now; more pull requests must not bury it.
    const p = projectWork(
      inputs({
        leases: null,
        worktrees: [worktree("/wt/busy", "busy"), worktree("/wt/stuck", "stuck")],
        pullRequests: [pr(1, "busy"), pr(2, "busy"), pr(3, "busy")],
        operations: { "/wt/stuck": op() },
      }),
    );
    expect(p.rows[0].key).toBe("/wt/stuck");
  });

  it("joins pull requests and runs to the worktree on that branch", () => {
    const p = projectWork(
      inputs({
        leases: null,
        worktrees: [worktree("/wt/alpha", "claude/alpha"), worktree("/wt/beta", "claude/beta")],
        pullRequests: [pr(7, "claude/alpha")],
        runs: [run(70, "claude/beta")],
      }),
    );
    expect(p.rows.find((r) => r.key === "/wt/alpha")!.pullRequests.map((x) => x.number)).toEqual([7]);
    expect(p.rows.find((r) => r.key === "/wt/beta")!.runs.map((x) => x.id)).toEqual([70]);
  });

  it("keeps a pull request on a branch no worktree holds in the catch-all", () => {
    const p = projectWork(
      inputs({ leases: null, worktrees: [worktree("/wt/alpha", "claude/alpha")], pullRequests: [pr(9, "orphan")] }),
    );
    const unbound = p.rows.find((r) => r.key === UNBOUND_ROW_ID)!;
    expect(unbound.pullRequests.map((x) => x.number)).toEqual([9]);
    expect(unbound.kind).toBe("unbound");
  });

  it("tallies verdicts per worktree, using the column it keys by", () => {
    const p = projectWork(
      inputs({
        leases: null,
        worktrees: [worktree("/wt/alpha", "a"), worktree("/wt/beta", "b")],
        events: [
          event({ worktree_path: "/wt/alpha", verdict_json: '{"status":"blocked"}' }),
          event({ worktree_path: "/wt/alpha", verdict_json: '{"status":"allowed"}' }),
          event({ worktree_path: "/wt/beta", verdict_json: '{"status":"warned"}' }),
        ],
      }),
    );
    const alpha = p.rows.find((r) => r.key === "/wt/alpha")!;
    expect(alpha.verdicts.events).toBe(2);
    expect(alpha.verdicts.byStatus.blocked).toBe(1);
    expect(p.rows.find((r) => r.key === "/wt/beta")!.verdicts.byStatus.warned).toBe(1);
  });

  it("does not conjure a row for a worktree git no longer lists", () => {
    // The ledger outlives the worktree. An event naming a removed directory
    // must not produce a row for a place that does not exist.
    const p = projectWork(
      inputs({
        leases: null,
        worktrees: [worktree("/wt/alpha", "a")],
        events: [event({ worktree_path: "/wt/deleted", verdict_json: '{"status":"blocked"}' })],
      }),
    );
    expect(p.rows.some((r) => r.key === "/wt/deleted")).toBe(false);
    expect(p.rows.find((r) => r.key === UNBOUND_ROW_ID)!.verdicts.byStatus.blocked).toBe(1);
  });

  it("leaves grants in the catch-all, since they are scoped to tasks", () => {
    const p = projectWork(
      inputs({ leases: null, worktrees: [worktree("/wt/alpha", "a")], grants: [grant("g1", "TASK-1")] }),
    );
    expect(p.rows.find((r) => r.key === "/wt/alpha")!.grants).toEqual([]);
    expect(p.rows.find((r) => r.key === UNBOUND_ROW_ID)!.grants).toHaveLength(1);
  });

  it("switches back to task rows the moment a store answers", () => {
    // Regression guard on the mode switch itself.
    const p = projectWork(
      inputs({
        leases: [lease("TASK-1")],
        worktrees: [worktree("/wt/alpha", "claude/alpha")],
        bindings: { "/wt/alpha": "TASK-1" },
      }),
    );
    expect(p.rows.map((r) => r.key)).toEqual(["TASK-1"]);
    expect(p.rows[0].kind).toBe("task");
    expect(p.rows[0].taskId).toBe("TASK-1");
  });
});

describe("parked operations belong to the worktree, not the row kind", () => {
  function op(kind = "Rebase" as const) {
    return {
      kind,
      current_step: 2,
      total_steps: 7,
      head_ref: "feature",
      incoming_ref: null,
      conflicted_paths: ["a.ts"],
      conflicted_total: 1,
      available: ["abort" as const],
      warnings: [],
    };
  }

  it("attaches a parked operation in task mode too", () => {
    // Task mode used to skip this assignment, so a DevCouncil repository
    // mid-rebase rendered as idle.
    const p = projectWork(
      inputs({
        leases: [lease("TASK-1")],
        worktrees: [worktree("/wt/alpha", "a"), worktree("/wt/beta", "b")],
        bindings: { "/wt/alpha": "TASK-1", "/wt/beta": "TASK-1" },
        operations: { "/wt/beta": op() },
      }),
    );
    expect(p.rows).toHaveLength(1);
    expect(p.rows[0].kind).toBe("task");
    expect(p.rows[0].operation?.kind).toBe("Rebase");
    expect(parkedWorktree(p.rows[0])?.worktree.path).toBe("/wt/beta");
    expect(openPathFor(p.rows[0])).toBe("/wt/beta");
  });

  it("opens the first worktree when none is parked", () => {
    const p = projectWork(
      inputs({
        leases: [lease("TASK-1")],
        worktrees: [worktree("/wt/alpha", "a"), worktree("/wt/beta", "b")],
        bindings: { "/wt/alpha": "TASK-1", "/wt/beta": "TASK-1" },
      }),
    );
    expect(openPathFor(p.rows[0])).toBe("/wt/alpha");
    expect(parkedWorktree(p.rows[0])).toBeNull();
  });

  it("puts a blocked task above a busier unblocked one", () => {
    const p = projectWork(
      inputs({
        leases: [lease("TASK-BUSY"), lease("TASK-STUCK")],
        worktrees: [worktree("/wt/busy", "busy"), worktree("/wt/stuck", "stuck")],
        bindings: { "/wt/busy": "TASK-BUSY", "/wt/stuck": "TASK-STUCK" },
        pullRequests: [pr(1, "busy"), pr(2, "busy"), pr(3, "busy")],
        operations: { "/wt/stuck": op() },
      }),
    );
    expect(p.rows[0].taskId).toBe("TASK-STUCK");
  });
});

describe("dirtyCount", () => {
  it("sums uncommitted files across a row's worktrees", () => {
    const p = projectWork(
      inputs({
        leases: null,
        worktrees: [{ ...worktree("/wt/a", "a"), dirty_files: 3 }],
      }),
    );
    expect(dirtyCount(p.rows[0])).toBe(3);
  });

  it("reports -1 rather than 0 when nothing was scanned", () => {
    // 0 means "clean"; -1 means "not counted". Collapsing them would render a
    // worktree past the scan cap as verified clean.
    const p = projectWork(
      inputs({ leases: null, worktrees: [{ ...worktree("/wt/a", "a"), dirty_files: null }] }),
    );
    expect(dirtyCount(p.rows[0])).toBe(-1);
  });
});

describe("insightSummary", () => {
  it("counts agent sessions from directory layout, not branch names", () => {
    const p = projectWork(
      inputs({
        leases: null,
        worktrees: [
          { ...worktree("/repo", "main"), is_main: true, dirty_files: 0 },
          { ...worktree("/repo/.claude/worktrees/s1", "claude/s1"), dirty_files: 2 },
          { ...worktree("/repo/wt/claude/handmade", "claude/handmade"), dirty_files: 1 },
        ],
      }),
    );
    const summary = insightSummary(p);
    expect(summary.worktrees).toBe(3);
    expect(summary.agentSessions).toBe(1);
    expect(summary.agentKinds).toEqual(["claude"]);
    expect(summary.dirtyWorktrees).toBe(2);
    expect(summary.unscannedDirty).toBe(0);
  });

  it("does not treat an unscanned dirty count as clean", () => {
    const p = projectWork(
      inputs({
        leases: null,
        worktrees: [{ ...worktree("/wt/a", "a"), dirty_files: null }],
      }),
    );
    const summary = insightSummary(p);
    expect(summary.dirtyWorktrees).toBe(0);
    expect(summary.unscannedDirty).toBe(1);
  });
});
