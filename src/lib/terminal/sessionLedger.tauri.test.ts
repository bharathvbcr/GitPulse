import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

const mocks = vi.hoisted(() => ({
  isTauri: vi.fn(() => true),
  invoke: vi.fn(),
  listen: vi.fn(),
}));

vi.mock("../platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../platform")>()),
  isTauri: mocks.isTauri,
}));
vi.mock("../ipc/invoke", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));

import {
  SESSION_LEDGER_EVENT,
  checkReopenSessionLedgerRow,
  dismissSessionLedgerRow,
  refreshSessionLedger,
  reopenSessionLedgerOffer,
  sessionLedgerStore,
  setSessionLedgerViewForTest,
  subscribeSessionLedger,
  type SessionLedgerOffer,
  type SessionLedgerView,
} from "./sessionLedger";

function offer(overrides: Partial<SessionLedgerOffer> = {}): SessionLedgerOffer {
  return {
    row_id: "row-1", terminal_id: "t", launcher: "claude", native_id: "n1", repo_path: "/r", start_dir: null,
    cwd: "/r", mode: null, started_at: 1, ended_at: 2, end_state: "app_quit", signal: null, resumable: true,
    reason: null, last_error: null, ...overrides,
  };
}

const view = (offers: SessionLedgerOffer[]): SessionLedgerView => ({ offers, reopenable_count: 0, unreadable_count: 0 });

/** A listen that records its handler and hands back an unlisten spy. */
function arrangeListen() {
  const unlisten = vi.fn();
  let handler: ((event: { payload: unknown }) => void) | undefined;
  mocks.listen.mockImplementation(async (_name: string, fn: (event: { payload: unknown }) => void) => {
    handler = fn;
    return unlisten;
  });
  return { unlisten, emit: (payload: unknown) => handler?.({ payload }) };
}

beforeEach(() => {
  mocks.isTauri.mockReturnValue(true);
  mocks.invoke.mockReset();
  mocks.listen.mockReset();
  setSessionLedgerViewForTest(view([]));
});

afterEach(() => {
  setSessionLedgerViewForTest(view([]));
});

describe("refreshSessionLedger", () => {
  it("returns the store as it stands outside the desktop shell, without asking the host", async () => {
    mocks.isTauri.mockReturnValue(false);
    setSessionLedgerViewForTest(view([offer()]));
    const read = await refreshSessionLedger();
    expect(read.offers).toHaveLength(1);
    expect(mocks.invoke).not.toHaveBeenCalled();
  });

  it("reads the durable ledger, recomputing the counts rather than trusting the payload's", async () => {
    mocks.invoke.mockResolvedValue({
      offers: [offer(), offer({ row_id: "row-2", resumable: false, end_state: "unreadable" })],
      reopenable_count: 99,
      unreadable_count: 99,
    });
    const read = await refreshSessionLedger();
    expect(mocks.invoke).toHaveBeenCalledWith("cmd_terminal_session_ledger");
    expect(read.reopenable_count).toBe(1);
    expect(read.unreadable_count).toBe(1);
    expect(get(sessionLedgerStore)).toEqual(read);
  });
});

describe("subscribeSessionLedger", () => {
  it("loads the snapshot on the first subscription, follows ledger events, and stops listening after the last unsubscribe", async () => {
    const { unlisten, emit } = arrangeListen();
    mocks.invoke.mockResolvedValue(view([offer()]));
    const stopA = subscribeSessionLedger();
    const stopB = subscribeSessionLedger();
    expect(mocks.listen).toHaveBeenCalledTimes(1);
    expect(mocks.listen).toHaveBeenCalledWith(SESSION_LEDGER_EVENT, expect.any(Function));
    await vi.waitFor(() => expect(get(sessionLedgerStore).offers).toHaveLength(1));

    emit({ offers: [offer({ row_id: "a" }), offer({ row_id: "b" })] });
    expect(get(sessionLedgerStore).offers.map((o) => o.row_id)).toEqual(["a", "b"]);
    // A malformed payload clears to an empty ledger rather than throwing in the listener.
    emit("garbage");
    expect(get(sessionLedgerStore).offers).toEqual([]);

    stopA();
    stopA();
    expect(unlisten).not.toHaveBeenCalled();
    stopB();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("drops a listener that resolves after every subscriber has gone", async () => {
    const unlisten = vi.fn();
    let release: ((fn: () => void) => void) | undefined;
    mocks.listen.mockImplementation(() => new Promise<() => void>((resolve) => { release = resolve; }));
    mocks.invoke.mockResolvedValue(view([]));
    const stop = subscribeSessionLedger();
    stop();
    release?.(unlisten);
    await vi.waitFor(() => expect(unlisten).toHaveBeenCalledTimes(1));
  });

  it("survives a listener that cannot be registered and a snapshot that cannot be read", async () => {
    mocks.listen.mockRejectedValue(new Error("no events"));
    mocks.invoke.mockRejectedValue(new Error("no ledger"));
    const stop = subscribeSessionLedger();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(get(sessionLedgerStore).offers).toEqual([]);
    expect(() => stop()).not.toThrow();
  });

  it("does not listen outside the desktop shell", () => {
    mocks.isTauri.mockReturnValue(false);
    const stop = subscribeSessionLedger();
    expect(mocks.listen).not.toHaveBeenCalled();
    stop();
  });
});

describe("dismissSessionLedgerRow", () => {
  it("asks the host to withdraw the row and shows the view it answers with", async () => {
    mocks.invoke.mockResolvedValue(view([offer({ row_id: "kept" })]));
    const next = await dismissSessionLedgerRow("gone");
    expect(mocks.invoke).toHaveBeenCalledWith("cmd_terminal_session_ledger_dismiss", { rowId: "gone" });
    expect(next.offers.map((o) => o.row_id)).toEqual(["kept"]);
    expect(get(sessionLedgerStore).offers.map((o) => o.row_id)).toEqual(["kept"]);
  });
});

describe("checkReopenSessionLedgerRow", () => {
  it("replaces the row it re-checked in place and counts again", async () => {
    setSessionLedgerViewForTest(view([offer({ row_id: "a" }), offer({ row_id: "b" })]));
    mocks.invoke.mockResolvedValue({ offer: offer({ row_id: "b", resumable: false, last_error: "cwd is gone" }) });
    await checkReopenSessionLedgerRow("b");
    expect(mocks.invoke).toHaveBeenCalledWith("cmd_terminal_session_ledger_reopen_check", { rowId: "b" });
    const now = get(sessionLedgerStore);
    expect(now.offers.map((o) => o.row_id)).toEqual(["a", "b"]);
    expect(now.offers[1].last_error).toBe("cwd is gone");
    expect(now.reopenable_count).toBe(1);
  });

  it("lists a row the store did not hold yet, first", async () => {
    setSessionLedgerViewForTest(view([offer({ row_id: "a" })]));
    mocks.invoke.mockResolvedValue({ offer: offer({ row_id: "new" }) });
    await checkReopenSessionLedgerRow("new");
    expect(get(sessionLedgerStore).offers.map((o) => o.row_id)).toEqual(["new", "a"]);
  });
});

describe("a failed reopen check keeps the row listed with its error", () => {
  it("records the error on the stored row", async () => {
    setSessionLedgerViewForTest(view([offer({ row_id: "a" }), offer({ row_id: "b" })]));
    const result = await reopenSessionLedgerOffer(offer({ row_id: "b" }), { check: async () => { throw new Error("host down"); } });
    expect(result.reopened).toBe(false);
    expect(result.error).toContain("host down");
    const stored = get(sessionLedgerStore).offers;
    expect(stored.find((o) => o.row_id === "b")?.last_error).toContain("host down");
    expect(stored.find((o) => o.row_id === "a")?.last_error).toBeNull();
  });
});
