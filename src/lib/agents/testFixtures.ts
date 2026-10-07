/**
 * Fixtures the Agents plane tests share. Test-only: no production module
 * imports this file.
 */

import type { InsightsSnapshot, WorktreeSummary } from "../insights/types";
import {
  projectAgentPlane,
  type PlaneInput,
  type PlaneProbe,
  type PlaneTask,
  type PlaneTerminal,
  type TaskProbe,
} from "./plane";

export const PATHS = { caseInsensitive: false };

export function worktree(fields: Partial<WorktreeSummary> = {}): WorktreeSummary {
  return {
    path: "/repo/.claude/worktrees/alpha",
    name: "alpha",
    branch: "agent/alpha",
    is_detached: false,
    is_main: false,
    is_bare: false,
    dirty_files: 0,
    agent_kind: "claude",
    session_slug: "alpha",
    operation_kind: "",
    operation_ok: true,
    ...fields,
  };
}

export function listing(items: WorktreeSummary[], fields: Partial<InsightsSnapshot["worktrees"]> = {}): InsightsSnapshot["worktrees"] {
  return {
    ok: true, error: "", count: items.length, scanned: items.length, dirty: 0, dirty_unknown: 0,
    blocked: 0, blocked_unknown: 0, truncated: false, items, ...fields,
  };
}

export function snapshot(fields: Partial<InsightsSnapshot> = {}): InsightsSnapshot {
  const worktrees = fields.worktrees ?? listing([worktree()]);
  const sessions = worktrees.items.filter((item) => item.agent_kind !== "").length;
  return {
    repo_path: "/repo",
    branch: "main",
    branch_ok: true,
    deadline_expired: false,
    duration_ms: 1,
    agents: { ok: true, sessions, kinds: [], truncated: false },
    changes: {
      ok: true, error: "", files: 0, staged: 0, unstaged: 0, untracked: 0, conflicted: 0,
      additions: 0, deletions: 0, churn_warnings: 0, churn_overflowed: false, truncated: false,
    },
    collisions: {
      ok: true, error: "", overlapping_files: 0, worktrees_involved: 0, scanned_worktrees: 1,
      unscanned_worktrees: 0, failed_worktrees: 0, truncated: false, items: [],
    },
    ledger: { recording: true, path: "", dropped: 0, error: "", error_code: "" },
    codeintel: { available: false, db_path: "" },
    ...fields,
    worktrees,
  };
}

/** One repository: its main checkout and two agent checkouts. */
export function family(): InsightsSnapshot {
  return snapshot({
    worktrees: listing([
      worktree({ path: "/repo", name: "repo", agent_kind: "", session_slug: "", is_main: true, branch: "main" }),
      worktree(),
      worktree({ path: "/repo/.claude/worktrees/beta", session_slug: "beta", name: "beta", branch: "agent/beta" }),
    ]),
  });
}

export function probe(fields: Partial<PlaneProbe> = {}): PlaneProbe {
  return {
    path: "/repo",
    label: "Repo",
    snapshot: snapshot(),
    error: "",
    skipped: false,
    skipReason: "",
    ...fields,
  };
}

export function terminal(fields: Partial<PlaneTerminal> = {}): PlaneTerminal {
  return {
    key: "t1",
    repoPath: "/repo",
    label: "Claude",
    title: "Claude",
    status: "running",
    sessionId: "s1",
    taskRunId: "",
    continuesRunId: "",
    cwd: null,
    attention: null,
    ...fields,
  };
}

export function task(fields: Partial<PlaneTask> = {}): PlaneTask {
  return {
    runId: "run-1",
    title: "Fix the gate",
    repoPath: "/repo",
    cwd: "/repo/.claude/worktrees/alpha",
    provider: "claude",
    tone: "quiet",
    disconnected: false,
    unstarted: false,
    pendingCount: 0,
    pendingMore: false,
    ...fields,
  };
}

export const tasks = (fields: Partial<TaskProbe> = {}): TaskProbe => ({
  ok: true, unread: false, error: "", complete: true, tasks: [], ...fields,
});

export const project = (fields: Partial<PlaneInput> = {}) =>
  projectAgentPlane({ probes: [probe()], terminals: [], tasks: null, paths: PATHS, ...fields });
