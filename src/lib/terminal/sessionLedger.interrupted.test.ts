import { describe, expect, it, vi } from "vitest";
import {
  hasUndismissedInterrupted,
  interruptedNoticeText,
  reopenAllSessionLedgerOffers,
  summarizeInterrupted,
  type SessionLedgerOffer,
} from "./sessionLedger";

const UUID_1 = "6f0a6d1e-1111-4222-8333-444455556661";
const UUID_2 = "6f0a6d1e-1111-4222-8333-444455556662";

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
    end_state: "lost",
    signal: null,
    resumable: true,
    reason: null,
    last_error: null,
    ...overrides,
  };
}

const repoA = makeOffer({ row_id: "a1", repo_path: "/work/a", cwd: "/work/a" });
const repoB = makeOffer({ row_id: "b1", repo_path: "/work/b", cwd: "/work/b", native_id: UUID_2 });
const gone = makeOffer({
  row_id: "c1",
  repo_path: "/work/c",
  native_id: null,
  resumable: false,
  reason: "no transcript",
});
const shell = makeOffer({ row_id: "sh", launcher: "shell", native_id: null, resumable: false });
const corrupt = makeOffer({ row_id: "bad", launcher: "unreadable", end_state: "unreadable", resumable: false });

function repoDeps() {
  return {
    showRepositorySurface: vi.fn(),
    snapshot: () => ({ openTabs: [{ id: "t", path: "/work/a" }], activeTabId: "t" }),
    activateTab: vi.fn(async () => {}),
    openRepo: vi.fn(async () => true),
    setTerminalOpen: vi.fn(() => true),
    identity: { caseInsensitive: false },
  };
}

describe("interrupted-agents notice logic", () => {
  it("summarizes agents across every repository, ignoring shell and corrupt rows", () => {
    const summary = summarizeInterrupted([repoA, repoB, gone, shell, corrupt]);
    expect(summary.agents.map((o) => o.row_id)).toEqual(["a1", "b1", "c1"]);
    expect(summary.resumable.map((o) => o.row_id)).toEqual(["a1", "b1"]);
    expect(summary.unresumable.map((o) => o.row_id)).toEqual(["c1"]);
    expect(interruptedNoticeText(summary)).toBe("3 agent sessions were interrupted. 1 cannot be resumed.");
    expect(interruptedNoticeText(summarizeInterrupted([repoA]))).toBe("1 agent session was interrupted.");
  });

  it("does not re-raise after dismissal for the same offers, but does for a new row", () => {
    const summary = summarizeInterrupted([repoA, repoB]);
    expect(hasUndismissedInterrupted(summary, new Set())).toBe(true);
    const dismissed = new Set(["a1", "b1"]);
    expect(hasUndismissedInterrupted(summary, dismissed)).toBe(false);
    expect(hasUndismissedInterrupted(summarizeInterrupted([repoA]), dismissed)).toBe(false);
    expect(hasUndismissedInterrupted(summarizeInterrupted([repoA, repoB, gone]), dismissed)).toBe(true);
  });
});

describe("latest incident only", () => {
  // The user's real ledger on 2026-11-24: 15 agents lost at 15:32 that day,
  // 17 more repaired from the 11-19 reboot five days earlier.
  const DAY = 24 * 3600 * 1000;
  const today = 1_795_559_524_000;
  const uuid = (n: number) => `6f0a6d1e-1111-4222-8333-${String(n).padStart(12, "0")}`;
  const lostAt = (prefix: string, at: number, n: number) =>
    Array.from({ length: n }, (_, i) =>
      makeOffer({ row_id: `${prefix}${i}`, native_id: uuid(prefix === "t" ? i : 100 + i), ended_at: at + i * 30 }),
    );
  const offers = [...lostAt("t", today, 15), ...lostAt("o", today - 5 * DAY, 17)];

  it("counts and reopens only the latest incident, and names the earlier ones", () => {
    const summary = summarizeInterrupted(offers);
    expect(summary.resumable).toHaveLength(15);
    expect(summary.earlier).toHaveLength(17);
    expect(interruptedNoticeText(summary)).toBe(
      "15 agent sessions were interrupted. 17 from earlier are in Review.",
    );
  });

  it("keeps a lone older incident whole when nothing newer exists", () => {
    const summary = summarizeInterrupted(lostAt("o", today - 5 * DAY, 17));
    expect(summary.resumable).toHaveLength(17);
    expect(summary.earlier).toHaveLength(0);
  });
});

describe("bounded Reopen all", () => {
  it("reopens one row at a time and never touches non-resumable rows", async () => {
    let active = 0;
    let peak = 0;
    const checked: string[] = [];
    const enqueue = vi.fn();
    const outcome = await reopenAllSessionLedgerOffers([repoA, gone, repoB, shell], {
      check: async (rowId) => {
        active += 1;
        peak = Math.max(peak, active);
        checked.push(rowId);
        await new Promise((resolve) => setTimeout(resolve, 5));
        active -= 1;
        return { offer: [repoA, repoB, gone].find((o) => o.row_id === rowId)! };
      },
      repo: repoDeps(),
      enqueue,
    });
    expect(peak).toBe(1);
    expect(checked).toEqual(["a1", "b1"]);
    expect(outcome.reopened).toBe(2);
    expect(enqueue).toHaveBeenCalledTimes(2);
  });

  it("skips a row another Reopen all is already handling", async () => {
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const enqueue = vi.fn();
    const deps = {
      check: async () => {
        await gate;
        return { offer: repoA };
      },
      repo: repoDeps(),
      enqueue,
    };
    const first = reopenAllSessionLedgerOffers([repoA], deps);
    const second = await reopenAllSessionLedgerOffers([repoA], deps);
    expect(second.skipped).toBe(1);
    release();
    await first;
    expect(enqueue).toHaveBeenCalledTimes(1);
  });
});
