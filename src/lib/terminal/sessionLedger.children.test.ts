import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import InterruptedAgentsNotice from "../components/InterruptedAgentsNotice.svelte";
import {
  interruptedNoticeText,
  normalizeSessionLedgerView,
  offerRows,
  reopenAllSessionLedgerOffers,
  setSessionLedgerViewForTest,
  summarizeInterrupted,
  topLevelOffers,
  type SessionLedgerOffer,
} from "./sessionLedger";
import {
  recentUncertainManagedRuns,
  refreshInterruptedManaged,
  setInterruptedManagedForTest,
  openInterruptedManaged,
} from "../workbench/interruptedManaged";
import type { TaskRun } from "../workbench/client";

function offer(row_id: string, overrides: Partial<SessionLedgerOffer> = {}): SessionLedgerOffer {
  return {
    row_id,
    terminal_id: null,
    launcher: "claude",
    native_id: `6f0a6d1e-1111-4222-8333-${row_id.padEnd(12, "0").slice(0, 12)}`,
    repo_path: "/work/a",
    start_dir: null,
    cwd: "/work/a",
    mode: null,
    started_at: 1,
    ended_at: 2,
    end_state: "lost",
    signal: null,
    resumable: true,
    reason: null,
    last_error: null,
    ...overrides,
  };
}

function run(id: string, overrides: Partial<TaskRun> = {}): TaskRun {
  return {
    kind: "managed", id, revision: 1, updated_at: 1000, task_id: `task-${id}`, source_revision: 1, task_title: "T",
    repository_id: "repo", provider: "claude", permission_mode: "ask", state: "exited", cwd: "/repo",
    created_at: 1, expires_at: 2, session_id: null, exit_code: null, reason: "", outcome_uncertain: true,
    finished_at: 1000,
    ...overrides,
  } as TaskRun;
}

afterEach(() => {
  setSessionLedgerViewForTest({ offers: [], reopenable_count: 0, unreadable_count: 0 });
  setInterruptedManagedForTest({ status: "unread", runs: [] });
});

describe("orchestrated children", () => {
  const parent = offer("p");
  const kidA = offer("ka", { parent_row_id: "p" });
  const kidB = offer("kb", { parent_row_id: "p" });
  const orphan = offer("o", { parent_row_id: "not-offered" });

  it("folds children under an offered parent and treats an absent parent as a normal offer", () => {
    expect(topLevelOffers([parent, kidA, kidB, orphan]).map((o) => o.row_id)).toEqual(["p", "o"]);
    expect(offerRows([kidA, parent, kidB]).map((r) => [r.offer.row_id, r.child])).toEqual([
      ["p", false], ["ka", true], ["kb", true],
    ]);
  });

  it("counts one entry for the parent and leaves the children out of Reopen all", async () => {
    const summary = summarizeInterrupted([parent, kidA, kidB]);
    expect(interruptedNoticeText(summary)).toBe("1 agent session was interrupted.");
    expect(summary.resumable.map((o) => o.row_id)).toEqual(["p"]);
    const check = vi.fn(async (rowId: string) => ({ offer: [parent, kidA, kidB].find((o) => o.row_id === rowId)! }));
    const repo = {
      showRepositorySurface: vi.fn(), snapshot: () => ({ openTabs: [{ id: "t", path: "/work/a" }], activeTabId: "t" }),
      activateTab: vi.fn(async () => {}), openRepo: vi.fn(async () => true), setTerminalOpen: vi.fn(() => true),
      identity: { caseInsensitive: false },
    };
    const enqueue = vi.fn();
    await reopenAllSessionLedgerOffers([parent, kidA, kidB], { check, repo, enqueue });
    expect(check.mock.calls.map((c) => c[0])).toEqual(["p"]);
  });

  it("survives a parent cycle without dropping rows", () => {
    const a = offer("a", { parent_row_id: "b" });
    const b = offer("b", { parent_row_id: "a" });
    expect(topLevelOffers([a, b]).map((o) => o.row_id)).toEqual(["a", "b"]);
  });
});

describe("unreconciled ledger", () => {
  it("normalizes reconcile_pending, absent as false", () => {
    expect(normalizeSessionLedgerView({ offers: [], reconcile_pending: true }).reconcile_pending).toBe(true);
    expect(normalizeSessionLedgerView({ offers: [] }).reconcile_pending ?? false).toBe(false);
  });

  it("does not stay silent while the ledger is unreconciled", () => {
    setSessionLedgerViewForTest({ offers: [], reopenable_count: 0, unreadable_count: 0, reconcile_pending: true });
    const { body } = render(InterruptedAgentsNotice);
    expect(body).toContain("interrupted-agents-notice");
    expect(body).toContain("check which agents survived yet");
    expect(body).toContain('data-tone="unknown"');
  });
});

describe("interrupted managed agents", () => {
  it("keeps only recent, managed, uncertain exits", () => {
    const now = 100_000;
    const kept = run("new", { finished_at: now - 60 });
    const old = run("old", { finished_at: now - 7 * 3600 });
    const observed = run("seen", { outcome_uncertain: false, finished_at: now - 60 });
    const terminal = run("term", { kind: "external_terminal", finished_at: now - 60 });
    expect(recentUncertainManagedRuns([old, observed, terminal, kept], now).map((r) => r.id)).toEqual(["new"]);
  });

  it("reports a failed read as failed, never as none", async () => {
    await refreshInterruptedManaged(async () => { throw new Error("store down"); }, 0);
    const { body } = render(InterruptedAgentsNotice);
    expect(body).toContain("interrupted-managed-unread");
  });

  it("counts them on their own line with an action that opens the first task", async () => {
    await refreshInterruptedManaged(async () => ({ runs: [run("a", { finished_at: 990 }), run("b", { finished_at: 995 })], complete: true }), 1000);
    const { body } = render(InterruptedAgentsNotice);
    expect(body).toContain("2 managed agents were interrupted");
    expect(body).toContain("interrupted-managed-open");
    const open = vi.fn();
    openInterruptedManaged([run("b"), run("a")], open);
    expect(open).toHaveBeenCalledWith("task-b");
  });
});
