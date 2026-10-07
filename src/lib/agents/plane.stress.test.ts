import { describe, expect, it } from "vitest";
import type { InsightsSnapshot, WorktreeSummary } from "../insights/types";
import {
  ATTENTION_REASONS,
  MAX_AGENT_ROWS,
  applyAgentFilter,
  planeHeadline,
  projectAgentPlane,
  type AgentRow,
  type AttentionReason,
  type PlaneProbe,
  type PlaneTask,
  type PlaneTerminal,
} from "./plane";

/**
 * The plane under mixtures no unit test would write down.
 *
 * Each seed builds probes, terminals and tasks independently, including the
 * combinations that look like a quiet workspace when a read did not happen:
 * a skip beside a failure, a null change count, a collision scan that stopped,
 * two processes in one checkout, a shell, an empty slug. The assertions are
 * the reporting rules, checked against the output rather than re-derived by
 * copying the projector.
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

const PATHS = { caseInsensitive: false };
const KINDS = ["claude", "codex", "grok", "cursor", ""];
const STATUSES = ["starting", "running", "exited", "closed"];
const ATTENTIONS = [null, "needs-you", "error", "signalled", "finished"] as const;

function snapshot(items: WorktreeSummary[], agentsOk: boolean, agentsTruncated: boolean, sessions: number, collision: "clear" | "partial" | "failed"): InsightsSnapshot {
  return {
    repo_path: "/repo",
    branch: "main",
    branch_ok: true,
    deadline_expired: false,
    duration_ms: 1,
    worktrees: {
      ok: true, error: "", count: items.length, scanned: items.length, dirty: 0, dirty_unknown: 0,
      blocked: 0, blocked_unknown: 0, truncated: agentsTruncated, items,
    },
    agents: { ok: agentsOk, sessions, kinds: [], truncated: agentsTruncated },
    changes: {
      ok: true, error: "", files: 0, staged: 0, unstaged: 0, untracked: 0, conflicted: 0,
      additions: 0, deletions: 0, churn_warnings: 0, churn_overflowed: false, truncated: false,
    },
    collisions: collision === "failed"
      ? { ok: false, error: "scan failed", overlapping_files: 0, worktrees_involved: 0, scanned_worktrees: 0, unscanned_worktrees: 0, failed_worktrees: 0, truncated: false, items: [] }
      : {
          ok: true, error: "", overlapping_files: 0, worktrees_involved: 0, scanned_worktrees: items.length,
          unscanned_worktrees: collision === "partial" ? 1 : 0, failed_worktrees: 0, truncated: false, items: [],
        },
    ledger: { recording: true, path: "", dropped: 0, error: "", error_code: "" },
    codeintel: { available: false, db_path: "" },
  };
}

describe("projectAgentPlane under arbitrary mixtures", () => {
  it("holds the reporting rules for every generated workspace", { timeout: 20_000 }, () => {
    let rowsSeen = 0;
    let floors = 0;
    let failedGaps = 0;
    for (let seed = 1; seed <= 400; seed += 1) {
      const random = mulberry32(seed);
      const pick = <T,>(items: readonly T[]): T => items[Math.floor(random() * items.length)];
      const repoCount = 1 + Math.floor(random() * 3);
      const probes: PlaneProbe[] = [];
      for (let repo = 0; repo < repoCount; repo += 1) {
        const path = `/repo-${repo}`;
        const mode = random();
        if (mode < 0.15) {
          probes.push({ path, label: `R${repo}`, snapshot: null, error: "", skipped: true, skipReason: random() < 0.5 ? "cap" : "deadline" });
          continue;
        }
        if (mode < 0.25) {
          probes.push({ path, label: `R${repo}`, snapshot: null, error: "down", skipped: false, skipReason: "" });
          continue;
        }
        const count = Math.floor(random() * 6);
        const items: WorktreeSummary[] = Array.from({ length: count }, (_, index) => {
          const slug = random() < 0.2 ? "" : `s${repo}-${index}`;
          const kind = pick(KINDS);
          const dirty = random() < 0.25 ? null : Math.floor(random() * 4);
          const operationOk = random() > 0.2;
          return {
            path: `/wt/${repo}/${index}`,
            name: slug || "container",
            branch: random() < 0.2 ? null : `b${index}`,
            is_detached: random() < 0.2,
            is_main: false,
            is_bare: false,
            dirty_files: dirty,
            agent_kind: kind,
            session_slug: slug,
            operation_kind: operationOk && random() < 0.3 ? "rebase" : "",
            operation_ok: operationOk,
          };
        });
        const collision = pick(["clear", "partial", "failed"] as const);
        probes.push({
          path,
          label: `R${repo}`,
          snapshot: snapshot(items, random() > 0.15, random() < 0.2, Math.floor(random() * 8), collision),
          error: "",
          skipped: false,
          skipReason: "",
        });
      }
      // A second probe of the first repository, sometimes a success after a failure.
      if (random() < 0.4 && probes[0]) {
        probes.push({ ...probes[0], error: random() < 0.5 ? "late" : probes[0].error, snapshot: random() < 0.5 ? null : probes[0].snapshot });
      }
      // Another tab of the first repository at a different path: one family
      // whose common directory was not read, so the sweep probed it twice.
      if (random() < 0.4 && probes[0]?.snapshot) {
        probes.push({ ...probes[0], path: "/wt/0/0", label: "R0 tab" });
      }

      const terminals: PlaneTerminal[] = Array.from({ length: Math.floor(random() * 5) }, (_, index) => ({
        key: random() < 0.1 ? "" : `term-${seed}-${index}`,
        repoPath: `/repo-${Math.floor(random() * repoCount)}`,
        label: random() < 0.3 ? "Shell" : "Claude",
        title: random() < 0.2 ? "" : `title ${index}`,
        status: pick(STATUSES),
        sessionId: `sid-${index}`,
        taskRunId: random() < 0.3 ? "run-shared" : "",
        continuesRunId: "",
        cwd: random() < 0.3
          ? null
          : `/wt/${Math.floor(random() * repoCount)}/${Math.floor(random() * 4)}${random() < 0.3 ? "/src/lib" : ""}`,
        attention: pick(ATTENTIONS),
      }));

      const tasks: PlaneTask[] = Array.from({ length: Math.floor(random() * 4) }, (_, index) => ({
        runId: random() < 0.15 ? "" : index === 0 ? "run-shared" : `run-${seed}-${index}`,
        title: `task ${index}`,
        repoPath: random() < 0.2 ? "" : `/repo-0`,
        cwd: random() < 0.4 ? `/wt/0/0` : `/missing/${index}`,
        provider: "claude",
        tone: pick([null, "quiet", "needs-you", "error", "problem", "active"] as const),
        disconnected: random() < 0.3,
        unstarted: random() < 0.2,
        pendingCount: random() < 0.3 ? 2 : 0,
        pendingMore: random() < 0.2,
      }));

      const plane = projectAgentPlane({
        probes,
        terminals,
        tasks: random() < 0.15
          ? null
          : random() < 0.25
            ? { ok: false, unread: true, error: "", complete: false, tasks: [] }
            : random() < 0.35
              ? { ok: false, unread: false, error: "store", complete: false, tasks: [] }
              : { ok: true, unread: false, error: "", complete: random() > 0.2, tasks },
        paths: PATHS,
      });

      rowsSeen += plane.rows.length;
      if (plane.checkoutsFloor || plane.tasksFloor || plane.rows.some((row) => row.parallelFloor)) floors += 1;
      failedGaps += plane.gaps.filter((gap) => gap.kind === "failed").length;

      expect(plane.shown).toBe(plane.rows.length);
      expect(plane.shown).toBeLessThanOrEqual(MAX_AGENT_ROWS);
      expect(plane.total).toBeGreaterThanOrEqual(plane.shown);
      expect(plane.truncated).toBe(plane.total > MAX_AGENT_ROWS);
      expect(new Set(plane.rows.map((row) => row.id)).size).toBe(plane.rows.length);
      const byRepo = new Map<string, AgentRow[]>();
      for (const row of plane.rows) {
        expect(row.session.length).toBeGreaterThan(0);
        expect(row.kind.length).toBeGreaterThan(0);
        expect(row.presenceDetail.length).toBeGreaterThan(0);
        if (row.presence === "on-disk") expect(row.presenceDetail).toContain("does not say a process is running");
        if (row.presence === "live") expect(row.liveKey).toBeTruthy();
        if (!row.dirtyKnown) expect(row.dirtyFiles).toBeNull();
        if (row.dirtyKnown) expect(row.dirtyFiles).not.toBeNull();
        for (let index = 1; index < row.attention.length; index += 1) {
          expect(rankOf(row.attention[index])).toBeGreaterThanOrEqual(rankOf(row.attention[index - 1]));
        }
        // A task whose repository was not resolved belongs to no repository.
        if (row.repoPath === "") {
          expect(row.parallelCount).toBe(1);
          expect(row.repoLabel).toBe("Repository not resolved");
          continue;
        }
        const list = byRepo.get(row.repoPath) ?? [];
        list.push(row);
        byRepo.set(row.repoPath, list);
      }
      for (const group of byRepo.values()) {
        expect(new Set(group.map((row) => row.parallelCount)).size).toBe(1);
        expect(new Set(group.map((row) => row.parallelFloor)).size).toBe(1);
        expect(group[0].parallelCount).toBeGreaterThanOrEqual(group.length);
      }
      for (const gap of plane.gaps) {
        expect(["failed", "skipped", "unread", "partial"]).toContain(gap.kind);
        expect(gap.reason.length).toBeGreaterThan(0);
      }
      const skipped = plane.gaps.filter((gap) => gap.kind === "skipped");
      const failed = plane.gaps.filter((gap) => gap.kind === "failed" && gap.repoPath);
      for (const gap of skipped) {
        expect(plane.rows.filter((row) => row.repoPath === gap.repoPath && row.presence === "on-disk")).toEqual([]);
      }
      for (const gap of failed) {
        expect(plane.rows.filter((row) => row.repoPath === gap.repoPath && row.presence === "on-disk")).toEqual([]);
      }
      // A null measurement is not a clean checkout, and a dirty count is not a request.
      for (const row of plane.rows) {
        const onlyDirty = row.attention.length > 0 && row.attention.every((reason) => reason === "dirty");
        if (onlyDirty) expect(applyAgentFilter([row], "attention")).toEqual([]);
        if (row.attention.includes("unmeasured")) expect(applyAgentFilter([row], "attention")).toHaveLength(1);
        if (row.attention.includes("needs-you")) expect(applyAgentFilter([row], "attention")).toHaveLength(1);
      }
      // A row is never both a process in this window and one not shown in it.
      for (const row of plane.rows) {
        if (row.presence === "live" || row.presence === "exited") expect(row.attention).not.toContain("disconnected");
      }
      // One row per checkout, however many tabs listed it.
      const onDisk = plane.rows.filter((row) => row.liveKey === null && row.presence === "on-disk").map((row) => row.worktreePath);
      expect(new Set(onDisk).size).toBe(onDisk.length);
      // The live count is the live rows, and the headline's two attention
      // numbers are exactly what the attention filter keeps.
      expect(plane.rows.filter((row) => row.presence === "live")).toHaveLength(plane.live);
      if (!plane.truncated) {
        expect(applyAgentFilter(plane.rows, "attention")).toHaveLength(plane.attention.needing + plane.attention.unread);
      }
      const taskGaps = plane.gaps.filter((gap) => gap.label === "Tasks");
      expect(taskGaps.length).toBeLessThanOrEqual(1);
      expect(plane.failed + plane.skipped + plane.read).toBe(plane.requested);
      expect(planeHeadline(plane, plane.rows.length)).not.toMatch(/\u0000/);
    }
    expect(rowsSeen).toBeGreaterThan(100);
    expect(floors).toBeGreaterThan(10);
    expect(failedGaps).toBeGreaterThan(10);
  });
});

function rankOf(reason: AttentionReason): number {
  const index = ATTENTION_REASONS.indexOf(reason);
  return index < 0 ? 99 : index;
}
