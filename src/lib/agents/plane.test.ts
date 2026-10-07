import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import type { InsightsSnapshot, WorktreeSummary } from "../insights/types";
import { isAgentWorktree } from "../work/agentWorktree";
import { readAgentCwds } from "./cwd";
import {
  MAX_AGENT_ROWS,
  applyAgentFilter,
  isAgentFilter,
  liveAgentCount,
  planeHeadline,
  projectAgentPlane,
  terminalInScope,
  type PlaneProbe,
  type PlaneTask,
  type PlaneTerminal,
  type TaskProbe,
} from "./plane";

const PATHS = { caseInsensitive: false };

function worktree(fields: Partial<WorktreeSummary> = {}): WorktreeSummary {
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

function snapshot(fields: Partial<InsightsSnapshot> = {}): InsightsSnapshot {
  const worktrees = fields.worktrees ?? {
    ok: true, error: "", count: 1, scanned: 1, dirty: 0, dirty_unknown: 0,
    blocked: 0, blocked_unknown: 0, truncated: false, items: [worktree()],
  };
  return {
    repo_path: "/repo",
    branch: "main",
    branch_ok: true,
    deadline_expired: false,
    duration_ms: 1,
    agents: { ok: true, sessions: 1, kinds: [{ kind: "claude", sessions: 1 }], truncated: false },
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

/** The default facets with a few fields changed, so a new facet field needs no edit here. */
const agentsFacet = (fields: Partial<InsightsSnapshot["agents"]>) => ({ ...snapshot().agents, ...fields });
const collisionsFacet = (fields: Partial<InsightsSnapshot["collisions"]>) => ({ ...snapshot().collisions, ...fields });

function probe(fields: Partial<PlaneProbe> = {}): PlaneProbe {
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

function terminal(fields: Partial<PlaneTerminal> = {}): PlaneTerminal {
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

function task(fields: Partial<PlaneTask> = {}): PlaneTask {
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

const tasks = (fields: Partial<TaskProbe> = {}): TaskProbe => ({
  ok: true, unread: false, error: "", complete: true, tasks: [], ...fields,
});

const project = (fields: Partial<Parameters<typeof projectAgentPlane>[0]> = {}) =>
  projectAgentPlane({ probes: [probe()], terminals: [], tasks: null, paths: PATHS, ...fields });

describe("projectAgentPlane", () => {
  it("reads a checkout as on disk, and does not call it a running process", () => {
    const plane = project();
    expect(plane.rows).toHaveLength(1);
    expect(plane.rows[0]).toMatchObject({
      presence: "on-disk",
      kind: "claude",
      session: "alpha",
      dirtyFiles: 0,
      dirtyKnown: true,
      liveKey: null,
    });
    expect(plane.rows[0].presenceDetail).toContain("does not say a process is running");
    expect(plane.read).toBe(1);
    expect(plane.gaps).toEqual([]);
  });

  it("keeps an agent checkout with no slug, and drops a path that is not an agent checkout", () => {
    const plane = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: 2, scanned: 2, dirty: 0, dirty_unknown: 0,
            blocked: 0, blocked_unknown: 0, truncated: false,
            items: [
              worktree({ session_slug: "", path: "/repo/.claude/worktrees" }),
              worktree({ agent_kind: "", session_slug: "nope", path: "/repo/human" }),
            ],
          },
        }),
      })],
    });
    // The slugless checkout is a worktree an agent layout holds. Fleet and the
    // Work view count it, so the plane shows it too. `/repo/human` is not one.
    expect(plane.rows).toHaveLength(1);
    expect(plane.rows[0]).toMatchObject({ worktreePath: "/repo/.claude/worktrees", kind: "claude", session: "claude" });
    expect(plane.checkouts).toBe(1);
    expect(plane.read).toBe(1);
  });

  it("keeps a failed probe out of the rows and distinct from a skip", () => {
    const failed = project({ probes: [probe({ snapshot: null, error: "unreachable" })] });
    expect(failed.rows).toEqual([]);
    expect(failed.failed).toBe(1);
    expect(failed.gaps.map((gap) => gap.kind)).toEqual(["failed"]);
    expect(failed.gaps[0].reason).toContain("unreachable");

    const unreadList = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: false, error: "worktrees failed", count: 0, scanned: 0, dirty: 0, dirty_unknown: 0,
            blocked: 0, blocked_unknown: 0, truncated: false,
            items: [worktree()],
          },
        }),
      })],
    });
    expect(unreadList.rows).toEqual([]);
    expect(unreadList.gaps[0].kind).toBe("failed");

    const skipped = project({ probes: [probe({ skipped: true, skipReason: "deadline", snapshot: snapshot() })] });
    expect(skipped.rows).toEqual([]);
    expect(skipped.skipped).toBe(1);
    expect(skipped.failed).toBe(0);
    expect(skipped.gaps[0].kind).toBe("skipped");
    expect(skipped.gaps[0].reason).toContain("time");
  });

  it("replaces an earlier failure or skip with a later read, and a skip does not hide a failure", () => {
    const good = probe();
    const bad = probe({ snapshot: null, error: "down" });
    const skip = probe({ skipped: true, skipReason: "cap", snapshot: null });
    expect(project({ probes: [bad, good] }).rows).toHaveLength(1);
    expect(project({ probes: [skip, good] }).gaps).toEqual([]);
    expect(project({ probes: [good, bad] }).rows).toHaveLength(1);
    const failedAfterSkip = project({ probes: [skip, bad] });
    expect(failedAfterSkip.rows).toEqual([]);
    expect(failedAfterSkip.gaps.map((gap) => gap.kind)).toEqual(["failed"]);
  });

  it("does not treat an unread change count as zero, or an unread operation as idle", () => {
    const plane = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: 1, scanned: 1, dirty: 0, dirty_unknown: 1,
            blocked: 0, blocked_unknown: 1, truncated: false,
            items: [worktree({ dirty_files: null, operation_ok: false, operation_kind: "merge" })],
          },
        }),
      })],
    });
    expect(plane.rows[0].dirtyFiles).toBeNull();
    expect(plane.rows[0].dirtyKnown).toBe(false);
    expect(plane.rows[0].attention).toEqual(["unprobed", "unmeasured"]);
    expect(plane.rows[0].attention).not.toContain("blocked");
    expect(plane.rows[0].attention).not.toContain("dirty");
  });

  it("names a parked operation only when the probe ran", () => {
    const plane = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: 1, scanned: 1, dirty: 1, dirty_unknown: 0,
            blocked: 1, blocked_unknown: 0, truncated: false,
            items: [worktree({ dirty_files: 3, operation_ok: true, operation_kind: "rebase" })],
          },
        }),
      })],
    });
    expect(plane.rows[0].attention).toEqual(["blocked", "dirty"]);
    expect(plane.rows[0].dirtyFiles).toBe(3);
  });

  it("does not report zero collisions when the scan did not cover every worktree", () => {
    const plane = project({
      probes: [probe({
        snapshot: snapshot({
          collisions: {
            ok: true, error: "", overlapping_files: 0, worktrees_involved: 0, scanned_worktrees: 0,
            unscanned_worktrees: 2, failed_worktrees: 0, truncated: false, items: [],
          },
        }),
      })],
    });
    expect(plane.rows[0].attention).toContain("unscanned");
    expect(plane.rows[0].attention).not.toContain("collision");
    expect(plane.gaps.some((gap) => gap.kind === "partial")).toBe(true);
  });

  it("marks a checkout that overlaps another, and only that checkout", () => {
    const plane = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: 2, scanned: 2, dirty: 0, dirty_unknown: 0,
            blocked: 0, blocked_unknown: 0, truncated: false,
            items: [
              worktree(),
              worktree({ path: "/repo/.claude/worktrees/beta", session_slug: "beta", name: "beta" }),
            ],
          },
          agents: { ok: true, sessions: 2, kinds: [], truncated: false },
          collisions: {
            ok: true, error: "", overlapping_files: 1, worktrees_involved: 1, scanned_worktrees: 2,
            unscanned_worktrees: 0, failed_worktrees: 0, truncated: false,
            items: [{ path: "a.ts", worktrees: [{ path: "/repo/.claude/worktrees/alpha", branch: null, agent_kind: "claude" }] }],
          },
        }),
      })],
    });
    const alpha = plane.rows.find((row) => row.session === "alpha");
    const beta = plane.rows.find((row) => row.session === "beta");
    expect(alpha?.attention).toContain("collision");
    expect(beta?.attention).not.toContain("collision");
    expect(beta?.attention).not.toContain("unscanned");
  });

  it("treats a capped agent list as a floor, not as the rows it managed to return", () => {
    const plane = project({
      probes: [probe({
        snapshot: snapshot({ agents: { ok: true, sessions: 9, kinds: [], truncated: true } }),
      })],
    });
    expect(plane.rows[0].parallelCount).toBe(9);
    expect(plane.rows[0].parallelFloor).toBe(true);
    expect(plane.checkoutsAreFloor).toBe(true);
    expect(planeHeadline(plane, plane.rows.length)).toMatch(/^at least /);
  });

  it("does not turn an unread agent summary into zero sessions", () => {
    const plane = project({
      probes: [probe({
        snapshot: snapshot({ agents: { ok: false, sessions: 0, kinds: [], truncated: false } }),
      })],
    });
    expect(plane.rows[0].parallelCount).toBe(1);
    expect(plane.rows[0].parallelFloor).toBe(true);
    expect(plane.gaps.some((gap) => gap.reason.includes("could not be read"))).toBe(true);
  });

  it("attaches one live process to its checkout and clones a second into its own row", () => {
    const plane = project({
      terminals: [
        terminal({ cwd: "/repo/.claude/worktrees/alpha", title: "first" }),
        terminal({ key: "t2", cwd: "/repo/.claude/worktrees/alpha", title: "second", sessionId: "s2" }),
      ],
    });
    expect(plane.rows).toHaveLength(2);
    expect(plane.rows.every((row) => row.presence === "live")).toBe(true);
    expect(new Set(plane.rows.map((row) => row.liveKey))).toEqual(new Set(["t1", "t2"]));
    expect(new Set(plane.rows.map((row) => row.id)).size).toBe(2);
    expect(plane.rows.every((row) => row.parallelCount === 2)).toBe(true);
    expect(plane.rows[0].presenceDetail).toContain("this window started");
  });

  it("does not bind an unknown directory to a checkout", () => {
    const plane = project({
      terminals: [terminal({ cwd: "/somewhere/else", repoPath: "/other", label: "Claude", title: "elsewhere" })],
    });
    expect(plane.rows).toHaveLength(2);
    const live = plane.rows.find((row) => row.presence === "live");
    const disk = plane.rows.find((row) => row.presence === "on-disk");
    expect(live?.worktreePath).toBe("/somewhere/else");
    expect(disk?.liveKey).toBeNull();
  });

  it("drops a shell, an exited process, and a finished bell, and keeps a process that still needs the reader", () => {
    const plane = project({
      terminals: [
        terminal({ key: "shell", label: "Shell", title: "zsh", cwd: "/repo", repoPath: "/repo" }),
        terminal({ key: "gone", status: "exited", attention: "finished", label: "Claude" }),
        terminal({ key: "ask", status: "exited", attention: "needs-you", label: "Claude", title: "asking", cwd: "/tmp/ask", repoPath: "/tmp/ask" }),
      ],
    });
    const sessions = plane.rows.map((row) => row.session);
    expect(sessions).not.toContain("zsh");
    expect(plane.rows.find((row) => row.liveKey === "gone")).toBeUndefined();
    expect(plane.rows.find((row) => row.liveKey === "ask")?.attention).toContain("needs-you");
  });

  it("keeps a shell that is sitting in an agent checkout", () => {
    expect(terminalInScope({ label: "Shell", repoPath: "/repo", cwd: "/repo/.claude/worktrees/alpha" })).toBe(true);
    expect(terminalInScope({ label: "Shell", repoPath: "/repo", cwd: "/repo" })).toBe(false);
    expect(terminalInScope({ label: "Claude", repoPath: "/repo", cwd: null })).toBe(true);
    expect(liveAgentCount([
      { key: "a", label: "Shell", status: "running", repoPath: "/repo" },
      { key: "b", label: "Claude", status: "running", repoPath: "/repo" },
      { key: "c", label: "Claude", status: "exited", repoPath: "/repo" },
      { key: "", label: "Claude", status: "running", repoPath: "/repo" },
    ])).toBe(1);
  });

  it("folds a task onto its attempt, and keeps a quiet attempt with no checkout off the plane (P2: kept)", () => {
    const folded = project({
      terminals: [terminal({ taskRunId: "run-1", cwd: "/repo/.claude/worktrees/alpha" })],
      tasks: tasks({ tasks: [task({ tone: "needs-you", pendingCount: 2 })] }),
    });
    expect(folded.rows).toHaveLength(1);
    expect(folded.rows[0].taskRunId).toBe("run-1");
    expect(folded.rows[0].attention).toEqual(["needs-you", "pending"]);

    const quiet = project({ tasks: tasks({ tasks: [task({ cwd: "/nowhere", tone: "quiet" })] }) });
    expect(quiet.rows).toHaveLength(1);
    expect(quiet.rows[0].taskRunId).toBeNull();

    const missing = project({
      tasks: tasks({ tasks: [task({ cwd: "/nowhere", disconnected: true, tone: "problem" })] }),
    });
    expect(missing.rows.map((row) => row.presence)).toEqual(["missing", "on-disk"]);
    const missingRow = missing.rows.find((row) => row.presence === "missing");
    expect(missingRow?.attention).toEqual(["disconnected"]);
    expect(missingRow?.attentionLabel).toBe("Running, not shown in this window");

    const broken = project({ tasks: tasks({ tasks: [task({ cwd: "/nowhere", tone: "problem" })] }) });
    expect(broken.rows.find((row) => row.presence === "missing")?.attention).toEqual(["error"]);
  });

  it("keeps an unread task list distinct from a failed one and from a capped one", () => {
    expect(project({ tasks: tasks({ unread: true, ok: false }) }).gaps.map((gap) => gap.kind)).toContain("unread");
    const failed = project({ tasks: tasks({ ok: false, error: "store down" }) });
    expect(failed.gaps.find((gap) => gap.label === "Tasks")?.kind).toBe("failed");
    const capped = project({ tasks: tasks({ complete: false, tasks: [] }) });
    expect(capped.tasksAreFloor).toBe(true);
    expect(capped.gaps.find((gap) => gap.label === "Tasks")?.kind).toBe("partial");
  });

  it("treats a capped pending page with no counted rows as still waiting", () => {
    const plane = project({
      tasks: tasks({ tasks: [task({ cwd: "/nowhere", tone: null, pendingCount: 0, pendingMore: true })] }),
    });
    expect(plane.rows.find((row) => row.presence === "missing")?.attention).toContain("pending");
  });

  it("says nothing about tasks when the caller is not watching them", () => {
    expect(project({ tasks: null }).gaps.find((gap) => gap.label === "Tasks")).toBeUndefined();
  });

  it("caps the rows it returns and still counts the rest", () => {
    const items = Array.from({ length: MAX_AGENT_ROWS + 5 }, (_, index) =>
      worktree({
        path: `/repo/.claude/worktrees/s${index}`,
        session_slug: `s${index}`,
        name: `s${index}`,
      }));
    const plane = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: items.length, scanned: items.length, dirty: 0, dirty_unknown: 0,
            blocked: 0, blocked_unknown: 0, truncated: false, items,
          },
          agents: { ok: true, sessions: items.length, kinds: [], truncated: false },
        }),
      })],
    });
    expect(plane.shown).toBe(MAX_AGENT_ROWS);
    expect(plane.total).toBe(MAX_AGENT_ROWS + 5);
    expect(plane.truncated).toBe(true);
    expect(planeHeadline(plane, plane.rows.length)).toContain("5 past the row cap");
    expect(new Set(plane.rows.map((row) => row.id)).size).toBe(plane.rows.length);
  });

  it("strips control characters and bounds the text it shows", () => {
    const plane = project({
      probes: [probe({
        label: "bad\u0000name",
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: 1, scanned: 1, dirty: 0, dirty_unknown: 0,
            blocked: 0, blocked_unknown: 0, truncated: false,
            items: [worktree({ session_slug: "a\nb\u0007", branch: "x".repeat(200) })],
          },
        }),
      })],
    });
    expect(plane.rows[0].repoLabel).toBe("bad name");
    expect(plane.rows[0].session).toBe("a b");
    expect(plane.rows[0].checkout.endsWith("…")).toBe(true);
    expect(plane.rows[0].checkout.length).toBeLessThanOrEqual(80);
  });

  it("agrees on the parallel count for a live process in the same repository", () => {
    const plane = project({
      probes: [probe({
        snapshot: snapshot({ agents: { ok: true, sessions: 4, kinds: [], truncated: false } }),
      })],
      terminals: [terminal({ cwd: "/repo", repoPath: "/repo", label: "Claude", title: "main tree" })],
    });
    expect(new Set(plane.rows.map((row) => row.parallelCount))).toEqual(new Set([4]));
    expect(plane.rows.every((row) => row.parallelFloor)).toBe(true);
  });
});

describe("checkout attribution", () => {
  const two = snapshot({
    worktrees: {
      ok: true, error: "", count: 3, scanned: 3, dirty: 0, dirty_unknown: 0,
      blocked: 0, blocked_unknown: 0, truncated: false,
      items: [
        worktree({ path: "/repo", name: "repo", agent_kind: "", session_slug: "", is_main: true, branch: "main" }),
        worktree(),
        worktree({ path: "/repo/.claude/worktrees/beta", session_slug: "beta", name: "beta" }),
      ],
    },
    agents: agentsFacet({ sessions: 2, kinds: [{ kind: "claude", sessions: 2 }] }),
  });

  it("P1: two tab paths of one repository yield one row per checkout", () => {
    // Both tabs belong to one family whose common directory was not read, so
    // the sweep probed both, and each snapshot lists every worktree.
    const plane = project({
      probes: [
        probe({ path: "/repo", label: "Repo", snapshot: two }),
        probe({ path: "/repo/.claude/worktrees/alpha", label: "alpha", snapshot: two }),
      ],
      terminals: [terminal({ repoPath: "/repo/.claude/worktrees/alpha", cwd: null, title: "in alpha" })],
    });
    expect(plane.rows.map((row) => row.worktreePath).sort()).toEqual([
      "/repo/.claude/worktrees/alpha",
      "/repo/.claude/worktrees/beta",
    ]);
    expect(new Set(plane.rows.map((row) => row.repoPath))).toEqual(new Set(["/repo"]));
    expect(plane.rows.every((row) => row.parallelCount === 2)).toBe(true);
    expect(plane.rows.find((row) => row.liveKey === "t1")?.worktreePath).toBe("/repo/.claude/worktrees/alpha");
    expect(plane.checkouts).toBe(2);
  });

  it("P1: two tabs of one repository give one set of notes, from the more complete listing", () => {
    const partial = snapshot({
      ...two,
      agents: agentsFacet({ sessions: 2, truncated: true }),
      collisions: collisionsFacet({ scanned_worktrees: 1, unscanned_worktrees: 2 }),
    });
    for (const order of [[0, 1], [1, 0]]) {
      const probes = [
        probe({ path: "/repo", label: "Repo", snapshot: two }),
        probe({ path: "/repo/.claude/worktrees/alpha", label: "alpha", snapshot: partial }),
      ];
      const plane = project({ probes: order.map((index) => probes[index]) });
      expect(plane.gaps).toEqual([]);
      expect(plane.checkoutsAreFloor).toBe(false);
      expect(plane.rows).toHaveLength(2);
      expect(new Set(plane.rows.map((row) => row.repoPath))).toEqual(new Set(["/repo"]));
      expect(plane.rows.every((row) => !row.attention.includes("unscanned") && !row.parallelFloor)).toBe(true);
      expect(plane.read).toBe(2);
    }
    const bothPartial = project({
      probes: [probe({ path: "/repo", snapshot: partial }), probe({ path: "/repo/.claude/worktrees/beta", snapshot: partial })],
    });
    expect(bothPartial.gaps.map((gap) => gap.repoPath)).toEqual(["/repo", "/repo"]);
    expect(bothPartial.gaps.map((gap) => gap.reason)).toEqual([
      "The worktree list was capped. Session counts for this repository are a floor.",
      "Collision scan did not cover every worktree. An empty overlap list is not a clear one.",
    ]);
  });

  it("P1: a second listing adds the worktrees the first did not see, under the same repository", () => {
    const first = snapshot({ ...two, worktrees: { ...two.worktrees, items: two.worktrees.items.slice(0, 2) } });
    const plane = project({
      probes: [probe({ path: "/repo", snapshot: first }), probe({ path: "/repo/.claude/worktrees/alpha", label: "alpha", snapshot: two })],
    });
    expect(plane.rows.map((row) => row.worktreePath).sort()).toEqual([
      "/repo/.claude/worktrees/alpha",
      "/repo/.claude/worktrees/beta",
    ]);
    expect(new Set(plane.rows.map((row) => row.repoPath))).toEqual(new Set(["/repo"]));
    expect(plane.checkouts).toBe(2);
  });

  it("P4: a terminal in a subdirectory of a checkout attaches to that checkout", () => {
    const plane = project({
      probes: [probe({ snapshot: two })],
      terminals: [terminal({ cwd: "/repo/.claude/worktrees/beta/src/lib", title: "deep" })],
    });
    expect(plane.rows).toHaveLength(2);
    const beta = plane.rows.find((row) => row.worktreePath === "/repo/.claude/worktrees/beta");
    expect(beta).toMatchObject({ presence: "live", liveKey: "t1" });
  });

  it("P3: a known cwd outside every checkout does not bind to the tab's checkout", () => {
    const plane = project({
      probes: [probe({ snapshot: two })],
      terminals: [terminal({ repoPath: "/repo/.claude/worktrees/alpha", cwd: "/elsewhere/project", label: "Codex", title: "away" })],
    });
    const alpha = plane.rows.find((row) => row.worktreePath === "/repo/.claude/worktrees/alpha");
    expect(alpha).toMatchObject({ presence: "on-disk", liveKey: null });
    const away = plane.rows.find((row) => row.liveKey === "t1");
    // Kind and checkout come from where the process is, not the tab it was opened from.
    expect(away).toMatchObject({ worktreePath: "/elsewhere/project", kind: "Codex" });
    expect(away?.checkout).not.toBe("Agent checkout");
  });

  it("P3: an unknown cwd still falls back to the tab's checkout", () => {
    const plane = project({
      probes: [probe({ snapshot: two })],
      terminals: [terminal({ repoPath: "/repo/.claude/worktrees/alpha", cwd: null })],
    });
    expect(plane.rows.find((row) => row.worktreePath === "/repo/.claude/worktrees/alpha")?.liveKey).toBe("t1");
  });

  it("P5: a task folds only into its own run's host, never an unrelated live terminal", () => {
    const plane = project({
      probes: [probe({ snapshot: two })],
      terminals: [terminal({ cwd: "/repo/.claude/worktrees/alpha", title: "someone else" })],
      tasks: tasks({ tasks: [task({ runId: "run-9", cwd: "/repo/.claude/worktrees/alpha", tone: "problem", disconnected: true })] }),
    });
    const live = plane.rows.find((row) => row.liveKey === "t1");
    expect(live?.taskRunId).toBeNull();
    expect(live?.attention).not.toContain("disconnected");
    const attempt = plane.rows.find((row) => row.taskRunId === "run-9");
    expect(attempt).toMatchObject({ presence: "missing", liveKey: null });
    for (const row of plane.rows) {
      expect(row.presence === "live" && row.attention.includes("disconnected")).toBe(false);
    }
  });

  it("P5: a task still folds into an on-disk checkout it is running in", () => {
    const plane = project({
      probes: [probe({ snapshot: two })],
      tasks: tasks({ tasks: [task({ runId: "run-9", cwd: "/repo/.claude/worktrees/alpha/src", tone: "problem", disconnected: true })] }),
    });
    expect(plane.rows).toHaveLength(2);
    expect(plane.rows.find((row) => row.worktreePath === "/repo/.claude/worktrees/alpha")).toMatchObject({
      presence: "on-disk",
      taskRunId: "run-9",
    });
  });

  it("P5: the terminal of a task's own run does not also call it disconnected", () => {
    const plane = project({
      terminals: [terminal({ taskRunId: "run-1", cwd: "/repo/.claude/worktrees/alpha" })],
      tasks: tasks({ tasks: [task({ disconnected: true, tone: "problem" })] }),
    });
    expect(plane.rows).toHaveLength(1);
    expect(plane.rows[0]).toMatchObject({ presence: "live", taskRunId: "run-1" });
    expect(plane.rows[0].attention).not.toContain("disconnected");
  });

  it("P6: a task row carries the repository it belongs to, not its working directory", () => {
    const plane = project({
      probes: [],
      tasks: tasks({ tasks: [task({ repoPath: "/repo", cwd: "/scratch/run-1", tone: "needs-you" })] }),
    });
    expect(plane.rows[0]).toMatchObject({ repoPath: "/repo", worktreePath: "/scratch/run-1" });

    const unresolved = project({
      probes: [],
      tasks: tasks({ tasks: [task({ repoPath: "", cwd: "/scratch/run-1", tone: "needs-you" })] }),
    });
    expect(unresolved.rows[0].repoPath).toBe("");
    expect(unresolved.rows[0].repoLabel).toBe("Repository not resolved");
  });

  it("an exited process that still needs the reader is not called live", () => {
    const plane = project({
      terminals: [terminal({ status: "exited", attention: "needs-you", cwd: "/repo/.claude/worktrees/alpha" })],
    });
    expect(plane.rows[0]).toMatchObject({ presence: "exited", liveKey: "t1" });
    expect(plane.live).toBe(0);
    expect(applyAgentFilter(plane.rows, "live")).toEqual([]);
  });

  it("counts each live process once, however many probes or keys repeat it", () => {
    const plane = project({
      probes: [probe({ snapshot: two }), probe({ path: "/repo/.claude/worktrees/beta", snapshot: two })],
      terminals: [
        terminal({ cwd: "/repo/.claude/worktrees/alpha" }),
        terminal({ cwd: "/repo/.claude/worktrees/alpha" }),
        terminal({ key: "shell", label: "Shell", cwd: "/repo/.claude/worktrees/beta/src" }),
        terminal({ key: "plain", label: "Shell", cwd: "/repo" }),
      ],
    });
    expect(plane.live).toBe(2);
  });
});

describe("a directory read by readAgentCwds", () => {
  it("binds the process to the checkout that contains it", async () => {
    const found = await readAgentCwds(["s1"], {
      read: async () => ({ process: "claude", busy: false, cwd: "/repo/.claude/worktrees/alpha/src", repo_dir: null }),
    });
    const plane = project({ terminals: [terminal({ repoPath: "/repo", cwd: found.get("s1") ?? null })] });
    expect(plane.rows).toHaveLength(1);
    expect(plane.rows[0]).toMatchObject({ worktreePath: "/repo/.claude/worktrees/alpha", presence: "live", liveKey: "t1" });
  });
});

describe("one definition of each agent count", () => {
  it("Fleet, the Work view, the plane and the tab chip agree on one fixture", () => {
    const items = [
      worktree({ path: "/repo", name: "repo", agent_kind: "", session_slug: "", is_main: true }),
      worktree(),
      worktree({ path: "/repo/.codex/worktrees/b", agent_kind: "codex", session_slug: "b", name: "b" }),
      worktree({ path: "/repo/.claude/worktrees", agent_kind: "claude", session_slug: "", name: "worktrees" }),
      worktree({ path: "/repo/feature", agent_kind: "", session_slug: "", name: "feature" }),
    ];
    // What `agent_summary` in insights/mod.rs computes: every item with a kind.
    const facetSessions = items.filter((item) => item.agent_kind !== "").length;
    const fixture = snapshot({
      worktrees: {
        ok: true, error: "", count: items.length, scanned: items.length, dirty: 0, dirty_unknown: 0,
        blocked: 0, blocked_unknown: 0, truncated: false, items,
      },
      agents: agentsFacet({ sessions: facetSessions, kinds: [] }),
    });
    const records = [
      { key: "a", label: "Claude", status: "running", repoPath: "/repo", sessionId: "s-a" },
      { key: "a", label: "Claude", status: "running", repoPath: "/repo", sessionId: "s-a" },
      { key: "b", label: "Shell", status: "running", repoPath: "/repo", sessionId: "s-b" },
      { key: "c", label: "Shell", status: "running", repoPath: "/repo", sessionId: "s-c" },
      { key: "d", label: "Codex", status: "exited", repoPath: "/repo", sessionId: "s-d" },
    ];
    const directories = new Map([["s-b", "/repo/.codex/worktrees/b/src"], ["s-c", "/repo"]]);
    const plane = project({
      probes: [probe({ snapshot: fixture })],
      terminals: records.map((record) => terminal({
        key: record.key,
        label: record.label,
        status: record.status,
        repoPath: record.repoPath,
        sessionId: record.sessionId,
        cwd: directories.get(record.sessionId) ?? null,
      })),
    });
    // Agent checkouts on disk: Fleet's facet, the Work view's tile, the plane.
    expect(plane.checkouts).toBe(facetSessions);
    expect(items.map((item) => item.path).filter(isAgentWorktree).length).toBe(facetSessions);
    // Live agent terminals: the tab chip and the plane's headline.
    expect(liveAgentCount(records, directories)).toBe(2);
    expect(plane.live).toBe(liveAgentCount(records, directories));
    expect(planeHeadline(plane, plane.rows.length)).toMatch(/^3 agent checkouts · 2 live terminals · /);
  });

  it("transcribes the facet's rule from the Rust that computes it", () => {
    // `facetSessions` above restates `agent_summary`: every listed worktree
    // with a kind counts, slug or none. If that rule changes, so must the plane.
    // insights/mod.rs runs the same fixture through the Rust in
    // `agent_summary_counts_every_agent_checkout_slug_or_none`.
    const rust = readFileSync(new URL("../../../src-tauri/src/insights/mod.rs", import.meta.url), "utf8");
    const body = rust.slice(rust.indexOf("fn agent_summary("), rust.indexOf("let sessions = counts"));
    expect(body).toMatch(/for item in items \{\s*if item\.agent_kind\.is_empty\(\) \{\s*continue;\s*\}/);
    expect(body).not.toContain("session_slug");
  });
});

describe("filters and the headline", () => {
  it("accepts only the four filters", () => {
    expect(isAgentFilter("all")).toBe(true);
    expect(isAgentFilter("attention")).toBe(true);
    expect(isAgentFilter("parallel")).toBe(true);
    expect(isAgentFilter("live")).toBe(true);
    expect(isAgentFilter("dirty")).toBe(false);
    expect(isAgentFilter("")).toBe(false);
  });

  it("leaves a dirty checkout out of the attention filter and keeps an unread count in it", () => {
    const dirty = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: 1, scanned: 1, dirty: 1, dirty_unknown: 0,
            blocked: 0, blocked_unknown: 0, truncated: false,
            items: [worktree({ dirty_files: 2 })],
          },
        }),
      })],
    });
    expect(applyAgentFilter(dirty.rows, "attention")).toEqual([]);
    expect(applyAgentFilter(dirty.rows, "all")).toHaveLength(1);
    expect(applyAgentFilter(dirty.rows, "live")).toEqual([]);

    const unread = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: 1, scanned: 1, dirty: 0, dirty_unknown: 1,
            blocked: 0, blocked_unknown: 0, truncated: false,
            items: [worktree({ dirty_files: null })],
          },
        }),
      })],
    });
    expect(applyAgentFilter(unread.rows, "attention")).toHaveLength(1);
  });

  it("counts sessions the filter hid, and does not call a cap a filter", () => {
    const plane = project();
    const visible = applyAgentFilter(plane.rows, "live");
    expect(planeHeadline(plane, visible.length)).toContain("1 hidden by the filter");
    expect(planeHeadline(plane, plane.rows.length)).not.toContain("hidden");
    expect(planeHeadline(plane, plane.rows.length)).toBe("1 agent checkout · 0 live terminals · 0 need attention");
    const watched = project({ tasks: tasks({ complete: false, tasks: [task({ cwd: "/nowhere", tone: "needs-you" })] }) });
    expect(planeHeadline(watched, watched.rows.length)).toBe("1 agent checkout · 0 live terminals · at least 1 task attempt · 1 need attention");
  });

  it("counts unread rows apart from rows that need the reader, and the filter holds both", () => {
    const items = [
      worktree(),
      worktree({ path: "/repo/.claude/worktrees/beta", session_slug: "beta", name: "beta" }),
      worktree({ path: "/repo/.claude/worktrees/gamma", session_slug: "gamma", name: "gamma", operation_ok: false }),
    ];
    const plane = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: 3, scanned: 3, dirty: 0, dirty_unknown: 0,
            blocked: 0, blocked_unknown: 1, truncated: false, items,
          },
          agents: agentsFacet({ sessions: 3, kinds: [] }),
          // A partial collision scan marks every checkout `unscanned`.
          collisions: collisionsFacet({ scanned_worktrees: 2, unscanned_worktrees: 1 }),
        }),
      })],
      terminals: [terminal({ cwd: "/repo/.claude/worktrees/alpha", attention: "needs-you" })],
    });
    const headline = planeHeadline(plane, plane.rows.length);
    expect(headline).toContain("1 need attention");
    expect(headline).toContain("2 not fully read");
    expect(applyAgentFilter(plane.rows, "attention")).toHaveLength(3);
    expect(plane.attention).toEqual({ needing: 1, unread: 2 });
  });

  it("counts repositories that were not read", () => {
    const plane = project({
      probes: [
        probe({ path: "/a", label: "A", snapshot: null, error: "no" }),
        probe({ path: "/b", label: "B", skipped: true, skipReason: "cap", snapshot: null }),
      ],
    });
    expect(planeHeadline(plane, 0)).toContain("2 repositories not read");
  });
});
