import { afterEach, describe, expect, it } from "vitest";
import type { InsightsSnapshot, WorktreeSummary } from "../insights/types";
import { ledgerStatus } from "../ledger/types";
import { setSignalledAsksForReader } from "../terminal/sessionActivity";
import {
  MAX_AGENT_ROWS,
  applyAgentFilter,
  isAgentFilter,
  liveAgentCount,
  planeHeadline,
  projectAgentPlane,
  rollupSummary,
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
    agents: { ok: true, sessions: 1, kinds: [{ kind: "claude", sessions: 1 }], truncated: false, live: { ok: true, sessions: 0, kinds: [], worktrees: [] } },
    changes: {
      ok: true, error: "", files: 0, staged: 0, unstaged: 0, untracked: 0, conflicted: 0,
      additions: 0, deletions: 0, churn_warnings: 0, churn_overflowed: false, truncated: false,
    },
    collisions: {
      ok: true, error: "", overlapping_files: 0, worktrees_involved: 0, scanned_worktrees: 1,
      unscanned_worktrees: 0, failed_worktrees: 0, truncated: false, shared_worktree_files: 0, shared_worktrees: [], sessions_ok: true, sessions_error: "", items: [],
    },
    ledger: ledgerStatus({ recording: true, path: "", dropped: 0, error: "", error_code: "" }),
    codeintel: { available: false, db_path: "" },
    ...fields,
    worktrees,
  };
}

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
    // The slugless checkout is a worktree an agent layout holds, and Fleet's
    // `agent_summary` counts it. `/repo/human` has no agent kind.
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
            unscanned_worktrees: 2, failed_worktrees: 0, truncated: false, shared_worktree_files: 0, shared_worktrees: [], sessions_ok: true, sessions_error: "", items: [],
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
          agents: { ok: true, sessions: 2, kinds: [], truncated: false, live: { ok: true, sessions: 0, kinds: [], worktrees: [] } },
          collisions: {
            ok: true, error: "", overlapping_files: 1, worktrees_involved: 1, scanned_worktrees: 2,
            unscanned_worktrees: 0, failed_worktrees: 0, truncated: false, shared_worktree_files: 0, shared_worktrees: [], sessions_ok: true, sessions_error: "",
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
        snapshot: snapshot({ agents: { ok: true, sessions: 9, kinds: [], truncated: true, live: { ok: true, sessions: 0, kinds: [], worktrees: [] } } }),
      })],
    });
    expect(plane.rows[0].parallelCount).toBe(9);
    expect(plane.rows[0].parallelFloor).toBe(true);
    expect(plane.sessionsAreFloor).toBe(true);
    expect(planeHeadline(plane, plane.rows.length)).toMatch(/^at least /);
  });

  it("does not turn an unread agent summary into zero sessions", () => {
    const plane = project({
      probes: [probe({
        snapshot: snapshot({ agents: { ok: false, sessions: 0, kinds: [], truncated: false, live: { ok: true, sessions: 0, kinds: [], worktrees: [] } } }),
      })],
    });
    expect(plane.rows[0].parallelCount).toBe(1);
    expect(plane.rows[0].parallelFloor).toBe(true);
    expect(plane.gaps.some((gap) => gap.reason.includes("couldn't be read"))).toBe(true);
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

  it("keeps informational finished notices in notifications and out of needing attention", () => {
    const plane = project({
      terminals: [
        terminal({
          key: "done",
          attention: "finished",
          attentionUrgency: "informational",
          attentionEvent: "turn_finished",
          attentionChannel: "hook",
          attentionLabel: "Finished its turn",
          attentionDetail: "Tests green",
          attentionAt: 1000,
          label: "Antigravity",
          launcher: "agy",
          title: "done",
          cwd: "/tmp/done",
          repoPath: "/tmp/done",
        }),
        terminal({
          key: "idle",
          sessionId: "s2",
          attention: "finished",
          attentionUrgency: "informational",
          attentionEvent: "idle_prompt",
          attentionChannel: "hook",
          attentionLabel: "Waiting for you",
          label: "Claude",
          launcher: "claude",
          title: "idle",
          cwd: "/tmp/idle",
          repoPath: "/tmp/idle",
        }),
        terminal({
          key: "ask",
          sessionId: "s3",
          attention: "needs-you",
          attentionUrgency: "blocking",
          attentionEvent: "permission_prompt",
          attentionChannel: "hook",
          attentionLabel: "Needs your permission",
          attentionDetail: "Bash: cargo test",
          attentionAt: 2000,
          label: "Claude",
          launcher: "claude",
          title: "asking",
          cwd: "/tmp/ask",
          repoPath: "/tmp/ask",
        }),
      ],
    });
    const done = plane.rows.find((row) => row.liveKey === "done");
    const idle = plane.rows.find((row) => row.liveKey === "idle");
    const ask = plane.rows.find((row) => row.liveKey === "ask");
    expect(done?.attention).toEqual(["finished"]);
    expect(done?.attentionLabel).toBe("Finished its turn");
    expect(done?.attentionDetail).toBe("Tests green");
    expect(done?.launcher).toBe("agy");
    expect(idle?.attention).toEqual(["idle-waiting"]);
    expect(idle?.attentionLabel).toBe("Idle, waiting for input");
    expect(ask?.attentionLabel).toBe("Needs your permission");
    expect(plane.attention.needing).toBe(1);
    expect(plane.notifications).toBe(2);
    expect(applyAgentFilter(plane.rows, "attention").map((row) => row.liveKey)).toEqual(["ask"]);
    expect(applyAgentFilter(plane.rows, "notifications").map((row) => row.liveKey)).toEqual(["done", "idle"]);
    expect(planeHeadline(plane, plane.rows.length)).toContain("1 need attention · 2 notifications");
  });

  it("routes passive OSC notices and legacy informational idle_prompt to notifications", () => {
    const plane = project({
      terminals: [
        terminal({
          key: "osc",
          sessionId: "s-osc",
          attention: "signalled",
          attentionUrgency: "informational",
          attentionChannel: "osc9",
          attentionDetail: "Build complete",
          cwd: "/tmp/osc",
          repoPath: "/tmp/osc",
        }),
        terminal({
          key: "legacy-idle",
          sessionId: "s-idle",
          attention: "needs-you",
          attentionUrgency: "informational",
          attentionEvent: "idle_prompt",
          cwd: "/tmp/legacy-idle",
          repoPath: "/tmp/legacy-idle",
        }),
      ],
    });
    expect(plane.attention.needing).toBe(0);
    expect(plane.notifications).toBe(2);
    expect(plane.rows.find((row) => row.liveKey === "osc")?.attention).toEqual(["notice"]);
    expect(plane.rows.find((row) => row.liveKey === "legacy-idle")?.attention).toEqual(["idle-waiting"]);
  });

  it("keeps a shell that is sitting in an agent checkout", () => {
    expect(terminalInScope({ label: "Shell", repoPath: "/repo", cwd: "/repo/.claude/worktrees/alpha" })).toBe(true);
    expect(terminalInScope({ label: "Shell", repoPath: "/repo", cwd: "/repo" })).toBe(false);
    expect(terminalInScope({ label: "Claude", repoPath: "/repo", cwd: null })).toBe(true);
    expect(liveAgentCount([
      { label: "Shell", status: "running", repoPath: "/repo" },
      { label: "Claude", status: "running", repoPath: "/repo" },
      { label: "Claude", status: "exited", repoPath: "/repo" },
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
    expect(capped.sessionsAreFloor).toBe(true);
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
          agents: { ok: true, sessions: items.length, kinds: [], truncated: false, live: { ok: true, sessions: 0, kinds: [], worktrees: [] } },
        }),
      })],
    });
    expect(plane.shown).toBe(MAX_AGENT_ROWS);
    expect(plane.total).toBe(MAX_AGENT_ROWS + 5);
    expect(plane.truncated).toBe(true);
    expect(plane.sessionsAreFloor).toBe(true);
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
        snapshot: snapshot({ agents: { ok: true, sessions: 4, kinds: [], truncated: false, live: { ok: true, sessions: 0, kinds: [], worktrees: [] } } }),
      })],
      terminals: [terminal({ cwd: "/repo", repoPath: "/repo", label: "Claude", title: "main tree" })],
    });
    expect(new Set(plane.rows.map((row) => row.parallelCount))).toEqual(new Set([4]));
    expect(plane.rows.every((row) => row.parallelFloor)).toBe(true);
  });
});

describe("filters and the headline", () => {
  it("accepts only the six valid agent filters", () => {
    expect(isAgentFilter("all")).toBe(true);
    expect(isAgentFilter("attention")).toBe(true);
    expect(isAgentFilter("notifications")).toBe(true);
    expect(isAgentFilter("gaps")).toBe(true);
    expect(isAgentFilter("parallel")).toBe(true);
    expect(isAgentFilter("live")).toBe(true);
    expect(isAgentFilter("dirty")).toBe(false);
    expect(isAgentFilter("")).toBe(false);
  });

  it("separates gaps filter for unread measurements while keeping unread in attention filter", () => {
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
    expect(applyAgentFilter(unread.rows, "gaps")).toHaveLength(1);
    expect(applyAgentFilter(unread.rows, "notifications")).toEqual([]);
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

  it("counts rows the filter hid, and does not call a cap a filter", () => {
    const plane = project();
    const visible = applyAgentFilter(plane.rows, "live");
    expect(planeHeadline(plane, visible.length)).toContain("1 hidden by the filter");
    expect(planeHeadline(plane, plane.rows.length)).not.toContain("hidden");
    expect(planeHeadline(plane, plane.rows.length)).toBe("1 agent checkout · 0 live terminals · 0 need attention");
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

  it("projects hook-reported activeSubagents onto rows and rolls up blocked subagents", () => {
    const plane = project({
      terminals: [
        terminal({
          key: "t-swarm",
          sessionId: "s-swarm",
          cwd: "/repo/.claude/worktrees/alpha",
          fullyIdle: false,
          activeSubagents: [
            { agentId: "sub-1", state: "working", event: "tool_finished", detail: "cargo check", changedMs: 100 },
            { agentId: "sub-2", state: "blocked", event: "permission_request", detail: "Bash: rm -rf", changedMs: 200 },
          ],
        }),
      ],
    });
    expect(plane.rows).toHaveLength(1);
    const row = plane.rows[0];
    expect(row.activeSubagents).toHaveLength(2);
    expect(row.fullyIdle).toBe(false);
    expect(row.rollup.subagents).toBe(2);
    expect(row.rollup.attention).toEqual(["needs-you"]);
    expect(rollupSummary(row)).toEqual({
      presence: "2 active subagents",
      attention: "Under it: Needs you",
    });
  });
});

describe("M3b iteration 2 attention projection regressions", () => {
  afterEach(() => setSignalledAsksForReader(false));

  it("isolates terminal attention across multiple terminals in the same agent checkout", () => {
    const cleanPlane = project({
      terminals: [
        terminal({
          key: "t1",
          sessionId: "s1",
          cwd: "/repo/.claude/worktrees/alpha",
          title: "first",
          attention: "needs-you",
          attentionUrgency: "blocking",
          attentionEvent: "permission_prompt",
          attentionChannel: "hook",
          attentionLabel: "Needs your permission",
        }),
        terminal({
          key: "t2",
          sessionId: "s2",
          cwd: "/repo/.claude/worktrees/alpha",
          title: "second",
          attention: "finished",
          attentionUrgency: "informational",
          attentionEvent: "turn_finished",
          attentionChannel: "hook",
          attentionLabel: "Finished its turn",
        }),
      ],
    });
    const r1 = cleanPlane.rows.find((row) => row.liveKey === "t1")!;
    const r2 = cleanPlane.rows.find((row) => row.liveKey === "t2")!;
    expect(r1.attention).toEqual(["needs-you"]);
    expect(r2.attention).toEqual(["finished"]);
    expect(r2.attentionLabel).toBe("Finished its turn");
    expect(cleanPlane.attention.needing).toBe(1);
    expect(cleanPlane.notifications).toBe(1);

    // Checkout-only reasons (like `dirty`) still carry onto cloned terminals without leaking t1's reasons.
    const dirtyPlane = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: 1, scanned: 1, dirty: 1, dirty_unknown: 0,
            blocked: 0, blocked_unknown: 0, truncated: false,
            items: [worktree({ dirty_files: 2 })],
          },
        }),
      })],
      terminals: [
        terminal({
          key: "t1",
          sessionId: "s1",
          cwd: "/repo/.claude/worktrees/alpha",
          title: "first",
          attention: "needs-you",
          attentionUrgency: "blocking",
          attentionEvent: "permission_prompt",
          attentionChannel: "hook",
          attentionLabel: "Needs your permission",
        }),
        terminal({
          key: "t2",
          sessionId: "s2",
          cwd: "/repo/.claude/worktrees/alpha",
          title: "second",
          attention: "finished",
          attentionUrgency: "informational",
          attentionEvent: "turn_finished",
          attentionChannel: "hook",
          attentionLabel: "Finished its turn",
        }),
      ],
    });
    const d2 = dirtyPlane.rows.find((row) => row.liveKey === "t2")!;
    expect(d2.attention).toEqual(["finished", "dirty"]);
    expect(d2.attentionLabel).toBe("Finished its turn · +1");
    expect(dirtyPlane.attention.needing).toBe(1);
    expect(dirtyPlane.notifications).toBe(1);
  });

  it("does not reuse stale terminal attentionLabel when foldTask elevates a row to a higher-priority reason", () => {
    const plane = project({
      terminals: [
        terminal({
          key: "t1",
          sessionId: "s1",
          taskRunId: "run-1",
          cwd: "/repo/.claude/worktrees/alpha",
          attention: "finished",
          attentionUrgency: "informational",
          attentionEvent: "turn_finished",
          attentionChannel: "hook",
          attentionLabel: "Finished its turn",
        }),
      ],
      tasks: tasks({
        tasks: [task({ runId: "run-1", tone: "needs-you" })],
      }),
    });
    expect(plane.rows).toHaveLength(1);
    expect(plane.rows[0].attention).toEqual(["needs-you", "finished"]);
    expect(plane.rows[0].attentionLabel).toBe("Needs you · +1");
  });

  it("does not duplicate finished alongside idle-waiting on task-backed live terminals receiving idle_prompt", () => {
    const plane = project({
      terminals: [
        terminal({
          key: "t1",
          sessionId: "s-task-idle",
          taskRunId: "run-idle-1",
          cwd: "/repo/.claude/worktrees/alpha",
          attention: "finished",
          attentionUrgency: "informational",
          attentionEvent: "idle_prompt",
          attentionChannel: "hook",
          attentionLabel: "Waiting for you",
        }),
      ],
      tasks: tasks({
        tasks: [task({ runId: "run-idle-1", tone: "finished" })],
      }),
    });
    expect(plane.rows).toHaveLength(1);
    expect(plane.rows[0].attention).toEqual(["idle-waiting"]);
    expect(plane.rows[0].attentionLabel).toBe("Idle, waiting for input");
    expect(plane.notifications).toBe(1);
  });

  it("suppresses bare BEL attention metadata on a dirty checkout when bell does not ask", () => {
    setSignalledAsksForReader(false);
    const plane = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: {
            ok: true, error: "", count: 1, scanned: 1, dirty: 1, dirty_unknown: 0,
            blocked: 0, blocked_unknown: 0, truncated: false,
            items: [worktree({ dirty_files: 2 })],
          },
        }),
      })],
      terminals: [
        terminal({
          key: "t-bell",
          sessionId: "s-bell",
          cwd: "/repo/.claude/worktrees/alpha",
          attention: "signalled",
          attentionUrgency: "informational",
          attentionEvent: null,
          attentionChannel: "bell",
          attentionLabel: "Signalled",
          attentionDetail: null,
          attentionAt: 1000,
        }),
      ],
    });
    expect(plane.rows).toHaveLength(1);
    expect(plane.rows[0].attention).toEqual(["dirty"]);
    expect(plane.rows[0].attentionLabel).toBe("Uncommitted changes");
    expect(plane.rows[0].attentionUrgency).toBeNull();
    expect(plane.rows[0].attentionChannel).toBeNull();
    expect(plane.rows[0].attentionAt).toBeNull();
    expect(plane.notifications).toBe(0);
    expect(plane.attention.needing).toBe(0);
  });

  it("derives blocking urgency for opted-in bare BEL when bell asks for reader", () => {
    setSignalledAsksForReader(true);
    const plane = project({
      terminals: [
        terminal({
          key: "t-bell",
          sessionId: "s-bell",
          cwd: "/repo/.claude/worktrees/alpha",
          attention: "signalled",
          attentionUrgency: "informational",
          attentionEvent: null,
          attentionChannel: "bell",
          attentionLabel: "Signalled",
          attentionDetail: null,
          attentionAt: 1000,
        }),
      ],
    });
    expect(plane.rows).toHaveLength(1);
    expect(plane.rows[0].attention).toEqual(["signalled"]);
    expect(plane.rows[0].attentionUrgency).toBe("blocking");
    expect(plane.attention.needing).toBe(1);
  });
});
