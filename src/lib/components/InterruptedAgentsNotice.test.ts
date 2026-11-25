import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import InterruptedAgentsNotice from "./InterruptedAgentsNotice.svelte";
import { setSessionLedgerViewForTest, type SessionLedgerOffer } from "../terminal/sessionLedger";

function offer(overrides: Partial<SessionLedgerOffer>): SessionLedgerOffer {
  return {
    row_id: "r",
    terminal_id: null,
    launcher: "claude",
    native_id: "6f0a6d1e-1111-4222-8333-444455556661",
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

afterEach(() => setSessionLedgerViewForTest({ offers: [], reopenable_count: 0, unreadable_count: 0 }));

describe("InterruptedAgentsNotice", () => {
  it("renders nothing when no agent was interrupted", () => {
    expect(render(InterruptedAgentsNotice).body).not.toContain("interrupted-agents-notice");
  });

  it("appears for offers in repositories with no open tab, with Reopen all and Review", () => {
    setSessionLedgerViewForTest({
      offers: [
        offer({ row_id: "a", repo_path: "/work/a" }),
        offer({ row_id: "b", repo_path: "/work/b" }),
        offer({ row_id: "c", repo_path: "/work/c", resumable: false, native_id: null, reason: "gone" }),
      ],
      reopenable_count: 2,
      unreadable_count: 0,
    });
    const { body } = render(InterruptedAgentsNotice);
    expect(body).toContain("3 agent sessions were interrupted. 1 cannot be resumed.");
    expect(body).toContain("interrupted-agents-reopen-all");
    expect(body).toContain("Reopen all (2)");
    expect(body).toContain("interrupted-agents-review");
  });

  it("omits Reopen all when nothing can be resumed", () => {
    setSessionLedgerViewForTest({
      offers: [offer({ row_id: "c", resumable: false, native_id: null, reason: "gone" })],
      reopenable_count: 0,
      unreadable_count: 0,
    });
    const { body } = render(InterruptedAgentsNotice);
    expect(body).toContain("1 cannot be resumed");
    expect(body).not.toContain("interrupted-agents-reopen-all");
  });
});
