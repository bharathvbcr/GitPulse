import { describe, expect, it } from "vitest";
import type { WorktreeSummary } from "../insights/types";
import { applyAgentFilter, liveAgentCount, projectAgentPlane, type PlaneProbe, type PlaneTask, type PlaneTerminal } from "./plane";
import { listing, PATHS, snapshot } from "./__tests__/fixtures";

/**
 * Attribution rules under generated workspaces: several repositories, each
 * read through one or more of its own checkouts, with terminals in random
 * directories (inside a checkout, below one, beside one, nowhere) and task
 * attempts whose runs may or may not have a terminal here.
 */

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const STATUSES = ["starting", "running", "exited"];
const ATTENTIONS = [null, "needs-you", "error", "signalled", "finished"] as const;
const TONES = [null, "quiet", "needs-you", "error", "problem", "active"] as const;

describe("projectAgentPlane attribution invariants", () => {
  it("holds for every generated workspace", { timeout: 20_000 }, () => {
    let duplicatedReads = 0;
    let foldedTasks = 0;
    let liveRows = 0;
    for (let seed = 1; seed <= 500; seed += 1) {
      const random = mulberry32(seed);
      const pick = <T,>(items: readonly T[]): T => items[Math.floor(random() * items.length)];
      const repoCount = 1 + Math.floor(random() * 3);
      const probes: PlaneProbe[] = [];
      const checkouts: string[] = [];
      for (let repo = 0; repo < repoCount; repo += 1) {
        const main = `/r${repo}`;
        const count = 1 + Math.floor(random() * 5);
        const agents = Array.from({ length: count }, (_, index) => `${main}/.claude/worktrees/s${index}`);
        checkouts.push(...agents);
        const items: WorktreeSummary[] = [
          { path: main, name: `r${repo}`, branch: "main", is_detached: false, is_main: true, is_bare: false,
            dirty_files: 0, agent_kind: "", session_slug: "", operation_kind: "", operation_ok: true },
          ...agents.map((path, index) => ({
            path, name: `s${index}`, branch: `b${index}`, is_detached: false, is_main: false, is_bare: false,
            dirty_files: random() < 0.2 ? null : 0, agent_kind: "claude", session_slug: `s${index}`,
            operation_kind: "", operation_ok: random() > 0.1,
          })),
        ];
        const read = snapshot({ worktrees: listing(items) });
        // The same repository opened through several of its own checkouts.
        const tabs = [main, ...agents.filter(() => random() < 0.3)];
        if (tabs.length > 1) duplicatedReads += 1;
        for (const tab of random() < 0.5 ? tabs : [...tabs].reverse()) {
          probes.push({ path: tab, label: tab, snapshot: read, error: "", skipped: false, skipReason: "" });
        }
      }

      const runIds = Array.from({ length: 4 }, (_, index) => `run-${index}`);
      const terminals: PlaneTerminal[] = Array.from({ length: Math.floor(random() * 6) }, (_, index) => {
        const where = random();
        const cwd = where < 0.25
          ? pick(checkouts)
          : where < 0.45
            ? `${pick(checkouts)}/src/deep`
            : where < 0.55
              ? `${pick(checkouts)}x`
              : where < 0.7 ? `/elsewhere/${index}` : null;
        return {
          key: random() < 0.1 ? "dup" : `term-${index}`,
          repoPath: random() < 0.5 ? `/r${Math.floor(random() * repoCount)}` : pick(checkouts),
          label: random() < 0.3 ? "Shell" : "Claude",
          title: `title ${index}`,
          status: pick(STATUSES),
          sessionId: `sid-${index}`,
          taskRunId: random() < 0.3 ? pick(runIds) : "",
          continuesRunId: random() < 0.1 ? pick(runIds) : "",
          cwd,
          attention: pick(ATTENTIONS),
        };
      });

      const tasks: PlaneTask[] = runIds.filter(() => random() < 0.7).map((runId) => ({
        runId,
        title: `task ${runId}`,
        repoPath: random() < 0.8 ? `/r${Math.floor(random() * repoCount)}` : "",
        cwd: random() < 0.6 ? `${pick(checkouts)}${random() < 0.3 ? "/sub" : ""}` : `/scratch/${runId}`,
        provider: pick(["claude", "codex"]),
        tone: pick(TONES),
        disconnected: random() < 0.5,
        unstarted: random() < 0.2,
        pendingCount: random() < 0.3 ? 1 : 0,
        pendingMore: false,
      }));

      const plane = projectAgentPlane({
        probes,
        terminals,
        tasks: { ok: true, unread: false, error: "", complete: true, tasks },
        paths: PATHS,
      });

      // P1: one checkout row per checkout, however many tabs read it.
      const fromCheckouts = plane.rows.filter((row) => row.origin === "checkout").map((row) => row.worktreePath);
      expect(new Set(fromCheckouts).size).toBe(fromCheckouts.length);
      expect(new Set(fromCheckouts)).toEqual(new Set(checkouts));
      expect(plane.checkouts).toBe(checkouts.length);
      // Every row of one repository is named for its main checkout.
      for (const row of plane.rows.filter((item) => item.origin === "checkout")) {
        expect(row.worktreePath.startsWith(`${row.repoPath}/`)).toBe(true);
      }

      // P5: a row is never both live and disconnected.
      for (const row of plane.rows) {
        expect(row.presence === "live" && row.attention.includes("disconnected")).toBe(false);
        if (row.presence === "live" || row.presence === "exited") expect(row.liveKey).toBeTruthy();
        if (row.presence === "live") liveRows += 1;
        if (row.origin === "checkout" && row.taskRunId && row.liveKey === null) foldedTasks += 1;
      }
      // Counts agree with the rows they describe.
      expect(plane.rows.filter((row) => row.presence === "live")).toHaveLength(plane.live);
      const records = terminals.map((item) => ({ ...item }));
      expect(plane.live).toBe(liveAgentCount(records, new Map(terminals.flatMap((item) => item.cwd ? [[item.sessionId, item.cwd]] : []))));
      expect(applyAgentFilter(plane.rows, "attention")).toHaveLength(plane.attention.needing + plane.attention.unread);
      expect(new Set(plane.rows.map((row) => row.id)).size).toBe(plane.rows.length);
    }
    expect(duplicatedReads).toBeGreaterThan(50);
    expect(foldedTasks).toBeGreaterThan(20);
    expect(liveRows).toBeGreaterThan(100);
  });
});
