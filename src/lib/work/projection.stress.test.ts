import { describe, expect, it } from "vitest";
import { projectWork, type WorkInputs, type WorkSources } from "./projection";
import type { WorktreeInfo } from "../branches/types";
import type { PullRequestInfo } from "../github/types";
import type { LedgerEvent } from "../ledger/types";
import type { RepoOperation } from "../repos/operation";

function sources(): WorkSources {
  const ok = { ok: true, present: true, detail: "" };
  return { tasks: ok, worktrees: ok, github: ok, ledger: ok, grants: ok };
}

function worktree(path: string, branch: string, dirty: number | null = 0): WorktreeInfo {
  return {
    path,
    name: path.split("/").pop() ?? path,
    head: "a".repeat(40),
    branch,
    is_bare: false,
    is_detached: false,
    is_main: false,
    is_locked: false,
    is_prunable: false,
    dirty_files: dirty,
  };
}

function op(): RepoOperation {
  return {
    kind: "Rebase",
    current_step: 1,
    total_steps: 3,
    head_ref: "feature",
    incoming_ref: null,
    conflicted_paths: ["a.ts"],
    conflicted_total: 1,
    available: ["abort"],
    warnings: [],
  };
}

describe("projectWork at agent-era scale", () => {
  it("joins hundreds of worktrees, PRs and events without dropping keys or inventing idle", () => {
    const n = 250;
    const worktrees = Array.from({ length: n }, (_, i) =>
      worktree(`/wt/${i}`, `feat/${i}`, i % 7 === 0 ? null : i % 5),
    );
    const operations: Record<string, RepoOperation | null> = {};
    for (let i = 0; i < n; i += 17) operations[`/wt/${i}`] = op();
    const pullRequests: PullRequestInfo[] = Array.from({ length: n }, (_, i) => ({
      number: i + 1,
      title: `PR ${i}`,
      state: "OPEN",
      head_ref: `feat/${i}`,
      base_ref: "main",
      url: `https://example.test/${i}`,
      is_draft: false,
      ci_status: "success",
      created_at: "",
      updated_at: "",
      review_decision: "",
      first_review_at: "",
    }));
    const events: LedgerEvent[] = Array.from({ length: n }, (_, i) => ({
      id: i,
      ulid: String(i).padStart(26, "0"),
      ts_utc: "2026-09-01T12:00:00Z",
      schema_version: 1,
      repo_path: "/repo",
      worktree_path: `/wt/${i}`,
      actor_kind: "agent",
      actor_id: null,
      session_id: null,
      task_id: null,
      action: "git.commit",
      object: null,
      argv_json: null,
      outcome: "ok",
      verdict_json: i % 11 === 0 ? "{ not json" : '{"status":"allowed"}',
      before_ref: null,
      after_ref: null,
      duration_ms: null,
      detail_json: null,
    }));

    const input: WorkInputs = {
      leases: null,
      titles: {},
      worktrees,
      bindings: {},
      pullRequests,
      runs: [],
      events,
      grants: [],
      operations,
      sources: sources(),
    };

    const p = projectWork(input);
    expect(p.rows.filter((r) => r.kind === "worktree")).toHaveLength(n);
    const keys = p.rows.map((r) => r.key);
    expect(new Set(keys).size).toBe(keys.length);
    expect(p.rows[0].operation?.kind).toBe("Rebase");
    const parked = p.rows.filter((r) => r.operation).length;
    expect(parked).toBe(Math.ceil(n / 17));
    const unparsed = p.rows.reduce((sum, r) => sum + r.verdicts.unparsed, 0);
    expect(unparsed).toBeGreaterThan(0);
    expect(p.rows.every((r) => r.verdicts.byStatus.allowed >= 0)).toBe(true);
  });
});
