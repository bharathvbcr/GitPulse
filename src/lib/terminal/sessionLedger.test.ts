import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import {
  describeEndState,
  dismissSessionLedgerRow,
  filterOffersForRepo,
  reopenAllSessionLedgerOffers,
  reopenBannerHeading,
  reopenSessionLedgerOffer,
  reopenableOffers,
  sessionLedgerStore,
  setSessionLedgerViewForTest,
  type SessionLedgerOffer,
} from "./sessionLedger";
import type { TaskTerminalRequest } from "./taskLaunches";

const UUID_1 = "6f0a6d1e-1111-4222-8333-444455556661";
const UUID_2 = "6f0a6d1e-1111-4222-8333-444455556662";
const UUID_3 = "6f0a6d1e-1111-4222-8333-444455556663";

function makeOffer(overrides: Partial<SessionLedgerOffer> = {}): SessionLedgerOffer {
  return {
    row_id: "row-1",
    terminal_id: "term-1",
    launcher: "claude",
    native_id: UUID_1,
    repo_path: "/work/repo",
    start_dir: null,
    cwd: "/work/repo",
    mode: "edit",
    started_at: 1000,
    ended_at: 2000,
    end_state: "app_quit",
    signal: null,
    resumable: true,
    reason: null,
    last_error: null,
    ...overrides,
  };
}

afterEach(() => {
  setSessionLedgerViewForTest({ offers: [], reopenable_count: 0, unreadable_count: 0 });
});

describe("sessionLedger helpers and filtering", () => {
  it("formats Reopen N agents headings and human end states", () => {
    expect(reopenBannerHeading(0)).toBe("Interrupted terminal sessions");
    expect(reopenBannerHeading(1)).toBe("Reopen 1 agent");
    expect(reopenBannerHeading(3)).toBe("Reopen 3 agents");

    expect(describeEndState("app_quit")).toBe("App quit");
    expect(describeEndState("lost")).toBe("Lost (crash or reboot)");
    expect(describeEndState("exited_signal", "Terminated: 15")).toBe("Signal (Terminated: 15)");
    expect(describeEndState("exited_signal", null)).toBe("Exited by signal");
    expect(describeEndState("unreadable")).toBe("unreadable");
    expect(describeEndState("exited_clean")).toBe("Exited");
    expect(describeEndState("running")).toBe("Running");
  });

  it("filters offers by repository while keeping unreadable rows visible everywhere", () => {
    const claudeA = makeOffer({ row_id: "row-a", repo_path: "/Work/Repo-A/" });
    const claudeB = makeOffer({ row_id: "row-b", repo_path: "/Work/Repo-B" });
    const unreadable = makeOffer({
      row_id: "corrupt-1",
      launcher: "unreadable",
      native_id: null,
      repo_path: "",
      cwd: "",
      end_state: "unreadable",
      resumable: false,
      reason: "unreadable",
    });

    const filtered = filterOffersForRepo([claudeA, claudeB, unreadable], "/work/repo-a", {
      caseInsensitive: true,
    });
    expect(filtered.map((o) => o.row_id)).toEqual(["row-a", "corrupt-1"]);
    expect(filterOffersForRepo([claudeA, claudeB], "/Work/Repo-A", { caseInsensitive: false }).map((o) => o.row_id)).toEqual([
      "row-a",
    ]);
  });

  it("counts only resumable rows with a native session id as reopenable", () => {
    const resumableClaude = makeOffer({ row_id: "row-c", resumable: true, native_id: UUID_1 });
    const shell = makeOffer({
      row_id: "row-s",
      launcher: "shell",
      native_id: null,
      resumable: false,
      reason: "not restorable",
    });
    const corrupt = makeOffer({
      row_id: "corrupt-x",
      end_state: "unreadable",
      native_id: null,
      resumable: false,
      reason: "unreadable",
    });
    const missingId = makeOffer({ row_id: "row-m", resumable: true, native_id: null });

    expect(reopenableOffers([resumableClaude, shell, corrupt, missingId]).map((o) => o.row_id)).toEqual(["row-c"]);
  });
});

describe("sessionLedger store dismissal and reopen lifecycle (S2-03 & S2-05)", () => {
  it("withdraws an offer only upon explicit dismiss, never silently", async () => {
    const first = makeOffer({ row_id: "row-1", native_id: UUID_1 });
    const second = makeOffer({ row_id: "row-2", native_id: UUID_2, end_state: "lost" });
    setSessionLedgerViewForTest({ offers: [first, second], reopenable_count: 2, unreadable_count: 0 });
    expect(get(sessionLedgerStore).reopenable_count).toBe(2);

    const after = await dismissSessionLedgerRow("row-1");
    expect(after.offers.map((o) => o.row_id)).toEqual(["row-2"]);
    expect(after.reopenable_count).toBe(1);
    expect(get(sessionLedgerStore).offers.map((o) => o.row_id)).toEqual(["row-2"]);
  });

  it("reopens a Claude offer with its recorded start_dir, permission mode, and ledgerRowId", async () => {
    const offer = makeOffer({
      row_id: "row-reopen-1",
      launcher: "claude",
      native_id: UUID_1,
      repo_path: "/work/repo",
      start_dir: "packages/api",
      cwd: "/work/repo/packages/api",
      mode: "bypass",
      end_state: "app_quit",
    });
    setSessionLedgerViewForTest({ offers: [offer], reopenable_count: 1, unreadable_count: 0 });

    const calls: string[] = [];
    const enqueued: TaskTerminalRequest[] = [];
    const outcome = await reopenSessionLedgerOffer(offer, {
      check: async (rowId) => {
        calls.push(`check:${rowId}`);
        return { offer };
      },
      repo: {
        showRepositorySurface: () => {
          calls.push("showRepositorySurface");
        },
        snapshot: () => ({
          openTabs: [{ id: "tab-1", path: "/work/repo" }],
          activeTabId: "tab-other",
        }),
        activateTab: async (id) => {
          calls.push(`activateTab:${id}`);
        },
        openRepo: async () => true,
        setTerminalOpen: (open) => {
          calls.push(`setTerminalOpen:${String(open)}`);
          return true;
        },
        identity: { caseInsensitive: false },
      },
      enqueue: (request) => {
        enqueued.push(request);
      },
    });

    expect(outcome).toEqual({ reopened: true, offer, error: null });
    expect(calls).toEqual([
      "check:row-reopen-1",
      "showRepositorySurface",
      "activateTab:tab-1",
      "setTerminalOpen:true",
    ]);
    expect(enqueued).toEqual([
      {
        runId: "",
        repoPath: "/work/repo",
        provider: "claude",
        title: "Claude",
        startDir: "packages/api",
        resume: {
          sessionId: UUID_1,
          mode: "bypass",
          ledgerRowId: "row-reopen-1",
        },
      },
    ]);
  });

  it("keeps the row listed with a per-row error when cwd is gone or reopen check fails", async () => {
    const goneError = "Recorded working directory /work/gone no longer exists.";
    const offer = makeOffer({ row_id: "row-gone", cwd: "/work/gone", end_state: "lost" });
    setSessionLedgerViewForTest({ offers: [offer], reopenable_count: 1, unreadable_count: 0 });

    const enqueued: TaskTerminalRequest[] = [];
    const checkFailedOffer: SessionLedgerOffer = {
      ...offer,
      resumable: false,
      reason: goneError,
      last_error: goneError,
    };
    const outcome = await reopenSessionLedgerOffer(offer, {
      check: async () => ({ offer: checkFailedOffer }),
      enqueue: (req) => enqueued.push(req),
    });

    expect(outcome.reopened).toBe(false);
    expect(outcome.error).toBe(goneError);
    expect(enqueued).toEqual([]);

    // Now test thrown error from checkFn also keeps row listed with error
    setSessionLedgerViewForTest({ offers: [offer], reopenable_count: 1, unreadable_count: 0 });
    const thrown = await reopenSessionLedgerOffer(offer, {
      check: async () => {
        throw new Error(goneError);
      },
      enqueue: (req) => enqueued.push(req),
    });
    expect(thrown.reopened).toBe(false);
    expect(thrown.error).toContain(goneError);
    expect(get(sessionLedgerStore).offers).toHaveLength(1);
    expect(get(sessionLedgerStore).offers[0].last_error).toContain(goneError);
  });

  it("refuses reopen on shell (not restorable) and corrupt (unreadable) rows", async () => {
    const shellOffer = makeOffer({
      row_id: "row-shell",
      launcher: "shell",
      native_id: null,
      resumable: false,
      reason: "not restorable",
    });
    const corruptOffer = makeOffer({
      row_id: "corrupt-1",
      launcher: "unreadable",
      native_id: null,
      end_state: "unreadable",
      resumable: false,
      reason: "unreadable",
    });

    const shellResult = await reopenSessionLedgerOffer(shellOffer, {
      check: async () => ({ offer: shellOffer }),
    });
    expect(shellResult).toEqual({ reopened: false, offer: shellOffer, error: "not restorable" });

    const corruptResult = await reopenSessionLedgerOffer(corruptOffer, {
      check: async () => ({ offer: corruptOffer }),
    });
    expect(corruptResult).toEqual({ reopened: false, offer: corruptOffer, error: "unreadable" });
  });

  it("reopens multiple app_quit Claude tabs via reopenAllSessionLedgerOffers while reporting per-row failures", async () => {
    const o1 = makeOffer({ row_id: "r1", native_id: UUID_1, mode: "edit" });
    const o2 = makeOffer({ row_id: "r2", native_id: UUID_2, mode: null });
    const o3Gone = makeOffer({ row_id: "r3", native_id: UUID_3, cwd: "/missing" });
    const shell = makeOffer({
      row_id: "r-shell",
      launcher: "shell",
      native_id: null,
      resumable: false,
      reason: "not restorable",
    });

    setSessionLedgerViewForTest({
      offers: [o1, o2, o3Gone, shell],
      reopenable_count: 3,
      unreadable_count: 0,
    });

    const enqueued: TaskTerminalRequest[] = [];
    const repo = {
      showRepositorySurface: vi.fn(),
      snapshot: () => ({ openTabs: [{ id: "tab-1", path: "/work/repo" }], activeTabId: "tab-1" }),
      activateTab: vi.fn(async () => {}),
      openRepo: vi.fn(async () => true),
      setTerminalOpen: vi.fn(() => true),
      identity: { caseInsensitive: false },
    };

    const summary = await reopenAllSessionLedgerOffers([o1, o2, o3Gone, shell], {
      check: async (rowId) => {
        if (rowId === "r3") {
          return {
            offer: {
              ...o3Gone,
              resumable: false,
              reason: "missing cwd",
              last_error: "missing cwd",
            },
          };
        }
        const match = [o1, o2].find((o) => o.row_id === rowId)!;
        return { offer: match };
      },
      repo,
      enqueue: (req) => enqueued.push(req),
    });

    expect(summary.reopened).toBe(2);
    expect(summary.failed).toBe(1);
    expect(enqueued.map((r) => [r.resume?.sessionId, r.resume?.mode, r.resume?.ledgerRowId])).toEqual([
      [UUID_1, "edit", "r1"],
      [UUID_2, null, "r2"],
    ]);
    expect(summary.results.find((r) => r.offer.row_id === "r3")?.error).toBe("missing cwd");
  });

  it("never lets the incident toast controller dismiss durable ledger rows (S2-05)", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const watchSource = readFileSync(join(here, "incidentWatch.ts"), "utf8");
    const incidentSource = readFileSync(join(here, "incident.ts"), "utf8");
    expect(watchSource).not.toContain("dismissSessionLedgerRow");
    expect(watchSource).not.toContain("setSessionLedgerView");
    expect(incidentSource).not.toContain("dismissSessionLedgerRow");
  });
});
