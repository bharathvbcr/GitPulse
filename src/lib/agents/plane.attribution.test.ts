import { describe, expect, it } from "vitest";
import { applyAgentFilter, kindLabel, uniqueRowId } from "./plane";
import { family, listing, probe, project, snapshot, task, tasks, terminal, worktree } from "./testFixtures";

const ALPHA = "/repo/.claude/worktrees/alpha";
const BETA = "/repo/.claude/worktrees/beta";

describe("P1: one repository read through two of its checkouts", () => {
  it("yields one row per checkout, named for the repository's main checkout", () => {
    // Two tabs of one family whose common directory was not read: the sweep
    // probed both, and each snapshot lists every worktree of the repository.
    const plane = project({
      probes: [
        probe({ path: ALPHA, label: "alpha", snapshot: family() }),
        probe({ path: "/repo", label: "Repo", snapshot: family() }),
      ],
      terminals: [terminal({ repoPath: ALPHA, cwd: null, title: "in alpha" })],
    });
    expect(plane.rows.map((row) => row.worktreePath).sort()).toEqual([ALPHA, BETA]);
    expect(new Set(plane.rows.map((row) => row.repoPath))).toEqual(new Set(["/repo"]));
    expect(new Set(plane.rows.map((row) => row.repoLabel))).toEqual(new Set(["Repo"]));
    expect(plane.rows.every((row) => row.parallelCount === 2)).toBe(true);
    expect(plane.rows.find((row) => row.liveKey === "t1")?.worktreePath).toBe(ALPHA);
    expect(plane.checkouts).toBe(2);
    expect(plane.read).toBe(2);
  });

  it("names a partial note once per repository, not once per tab", () => {
    const partial = snapshot({
      worktrees: family().worktrees,
      collisions: {
        ok: true, error: "", overlapping_files: 0, worktrees_involved: 0, scanned_worktrees: 1,
        unscanned_worktrees: 2, failed_worktrees: 0, truncated: false, items: [],
      },
    });
    const plane = project({ probes: [probe({ snapshot: partial }), probe({ path: BETA, label: "beta", snapshot: partial })] });
    expect(plane.gaps.filter((gap) => gap.kind === "partial")).toHaveLength(1);
  });
});

describe("P3/P4: a terminal binds to the checkout that contains its directory", () => {
  it("attaches a terminal in a subdirectory of a checkout to that checkout", () => {
    const plane = project({
      probes: [probe({ snapshot: family() })],
      terminals: [terminal({ cwd: `${BETA}/src/lib`, title: "deep" })],
    });
    expect(plane.rows).toHaveLength(2);
    expect(plane.rows.find((row) => row.worktreePath === BETA)).toMatchObject({ presence: "live", liveKey: "t1" });
  });

  it("does not take a sibling that only shares a name prefix", () => {
    const plane = project({
      probes: [probe({ snapshot: family() })],
      terminals: [terminal({ cwd: `${ALPHA}bet/src`, title: "neighbour" })],
    });
    expect(plane.rows.find((row) => row.worktreePath === ALPHA)?.liveKey).toBeNull();
  });

  it("does not bind a known directory outside every checkout to the tab's checkout", () => {
    const plane = project({
      probes: [probe({ snapshot: family() })],
      terminals: [terminal({ repoPath: ALPHA, cwd: "/elsewhere/project", label: "Codex", title: "away" })],
    });
    expect(plane.rows.find((row) => row.worktreePath === ALPHA)).toMatchObject({ presence: "on-disk", liveKey: null });
    const away = plane.rows.find((row) => row.liveKey === "t1");
    // Kind and checkout come from where the process is, not from the tab it was opened from.
    expect(away).toMatchObject({ worktreePath: "/elsewhere/project", kind: "Codex" });
    expect(away?.checkout).not.toBe("Agent checkout");
    expect(away?.checkout).not.toBe("Main checkout");
  });

  it("falls back to the tab's checkout only when the directory is unknown", () => {
    const plane = project({
      probes: [probe({ snapshot: family() })],
      terminals: [terminal({ repoPath: ALPHA, cwd: null })],
    });
    expect(plane.rows.find((row) => row.worktreePath === ALPHA)?.liveKey).toBe("t1");
  });

  it("does not count a shell as an agent once its directory has left the agent checkout", () => {
    const plane = project({
      probes: [probe({ snapshot: family() })],
      terminals: [terminal({ repoPath: ALPHA, cwd: "/tmp", label: "Shell", title: "zsh" })],
    });
    expect(plane.rows.some((row) => row.liveKey === "t1")).toBe(false);
  });
});

describe("P5: a task folds only into its own run's host", () => {
  it("never into an unrelated live terminal", () => {
    const plane = project({
      probes: [probe({ snapshot: family() })],
      terminals: [terminal({ cwd: ALPHA, title: "someone else" })],
      tasks: tasks({ tasks: [task({ runId: "run-9", cwd: ALPHA, tone: "problem", disconnected: true })] }),
    });
    const live = plane.rows.find((row) => row.liveKey === "t1");
    expect(live?.taskRunId).toBeNull();
    expect(live?.attention).not.toContain("disconnected");
    expect(plane.rows.find((row) => row.taskRunId === "run-9")).toMatchObject({ presence: "missing", liveKey: null });
    for (const row of plane.rows) {
      expect(row.presence === "live" && row.attention.includes("disconnected")).toBe(false);
    }
  });

  it("still into the on-disk checkout it is running in, subdirectory included", () => {
    const plane = project({
      probes: [probe({ snapshot: family() })],
      tasks: tasks({ tasks: [task({ runId: "run-9", cwd: `${ALPHA}/src`, tone: "problem", disconnected: true })] }),
    });
    expect(plane.rows).toHaveLength(2);
    expect(plane.rows.find((row) => row.worktreePath === ALPHA)).toMatchObject({ presence: "on-disk", taskRunId: "run-9" });
  });

  it("does not call its own run's terminal disconnected", () => {
    const plane = project({
      terminals: [terminal({ continuesRunId: "run-1", cwd: ALPHA })],
      tasks: tasks({ tasks: [task({ disconnected: true, tone: "problem" })] }),
    });
    expect(plane.rows).toHaveLength(1);
    expect(plane.rows[0]).toMatchObject({ presence: "live", taskRunId: "run-1" });
    expect(plane.rows[0].attention).not.toContain("disconnected");
  });
});

describe("P6: a task row names its repository, not its directory", () => {
  it("carries the repository path it was given and keeps the directory as the checkout", () => {
    const plane = project({
      probes: [],
      tasks: tasks({ tasks: [task({ repoPath: "/repo", cwd: "/scratch/run-1", tone: "needs-you" })] }),
    });
    expect(plane.rows[0]).toMatchObject({ repoPath: "/repo", worktreePath: "/scratch/run-1" });
  });

  it("says when the repository could not be resolved instead of using the directory", () => {
    const plane = project({
      probes: [],
      tasks: tasks({ tasks: [task({ repoPath: "", cwd: "/scratch/run-1", tone: "needs-you" })] }),
    });
    expect(plane.rows[0].repoPath).toBe("");
    expect(plane.rows[0].repoLabel).toBe("Repository not resolved");
  });

  it("joins the repository a read probe already names", () => {
    const plane = project({
      probes: [probe({ snapshot: family() })],
      tasks: tasks({ tasks: [task({ repoPath: "/repo", cwd: "/scratch/run-1", tone: "needs-you" })] }),
    });
    const attempt = plane.rows.find((row) => row.taskRunId === "run-1");
    expect(attempt).toMatchObject({ repoPath: "/repo", repoLabel: "Repo" });
    expect(new Set(plane.rows.map((row) => row.parallelCount))).toEqual(new Set([3]));
  });
});

describe("rows backed by a task run", () => {
  it("show the run's provider as kind and the task title as name, over the lane's own kind", () => {
    const lane = "/repo/.gitpulse/worktrees/fix-gate-1a2b";
    const plane = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: listing([worktree({ path: lane, agent_kind: "gitpulse", session_slug: "fix-gate-1a2b", name: "fix-gate-1a2b" })]),
        }),
      })],
      terminals: [terminal({ cwd: lane, taskRunId: "run-1", label: "Codex", title: "Codex" })],
      tasks: tasks({ tasks: [task({ cwd: lane, provider: "codex", title: "Fix the gate" })] }),
    });
    expect(plane.rows).toHaveLength(1);
    expect(plane.rows[0]).toMatchObject({ kind: "codex", session: "Fix the gate", taskRunId: "run-1", liveKey: "t1" });
    expect(kindLabel(plane.rows[0].kind)).toBe("Codex");
  });

  it("label kinds for people and leave an unknown one as it was found", () => {
    expect(kindLabel("claude")).toBe("Claude Code");
    expect(kindLabel("Codex")).toBe("Codex");
    expect(kindLabel("GitPulse")).toBe("GitPulse task");
    expect(kindLabel("shell")).toBe("Shell");
    expect(kindLabel("agy")).toBe("agy");
  });
});

describe("presence says what the process is doing", () => {
  it("does not call an exited process that still needs the reader live", () => {
    const plane = project({
      terminals: [terminal({ status: "exited", attention: "needs-you", cwd: ALPHA })],
    });
    expect(plane.rows[0]).toMatchObject({ presence: "exited", liveKey: "t1" });
    expect(plane.rows[0].attention).toContain("needs-you");
    expect(plane.live).toBe(0);
    expect(applyAgentFilter(plane.rows, "live")).toEqual([]);
  });
});

describe("row ids", () => {
  it("never reuse an id when the fallback suffix is already taken", () => {
    // The old fallback appended the set's size, which is "x|2" here: taken.
    const used = new Set(["x", "x|2"]);
    const id = uniqueRowId("x", used);
    expect(used.has(id)).toBe(false);
    expect(uniqueRowId("y", used)).toBe("y");
  });

  it("stay unique across a projection with repeated keys", () => {
    const plane = project({
      probes: [],
      terminals: [terminal({ key: "k", cwd: "/x" }), terminal({ key: "k|1", cwd: "/x" })],
      tasks: tasks({ tasks: [task({ runId: "r", repoPath: "", cwd: "/y", tone: "needs-you" })] }),
    });
    expect(new Set(plane.rows.map((row) => row.id)).size).toBe(plane.rows.length);
  });
});
