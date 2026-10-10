import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
const answer = (value: unknown) => invoke.mockResolvedValue(JSON.stringify(value));
const sent = () => invoke.mock.calls.map(([, args]) => JSON.parse((args as { input: string }).input));

import { MAX_VERDICT_BATCH, getVerdicts } from "./verification";

beforeEach(() => invoke.mockReset());

describe("getVerdicts", () => {
  it("reads every card in one request, deduplicated, and names only verified cards", async () => {
    answer({ ok: true, items: { a: { ok: true, state: "failed", stale: false }, b: { ok: true, state: "passed", stale: true } } });
    const out = await getVerdicts(["a", "b", "c", "a"]);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke.mock.calls[0][1]).toMatchObject({ method: "verifications.board" });
    expect(sent()).toEqual([{ ids: ["a", "b", "c"] }]);
    expect([...out]).toEqual([["a", { state: "failed", stale: false }], ["b", { state: "passed", stale: true }]]);
  });

  it("makes no request for an empty board", async () => {
    expect((await getVerdicts([])).size).toBe(0);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("fails loudly when the read fails, rather than answering none", async () => {
    invoke.mockRejectedValueOnce({ code: "store_error", message: "store unavailable" });
    const outcome = await getVerdicts(["a"]).then(() => "resolved", (error: unknown) => `rejected: ${error instanceof Error ? error.message : String(error)}`);
    expect(outcome).toContain("rejected");
    expect(outcome).toContain("store unavailable");
  });

  it("refuses a malformed answer and an unknown state", async () => {
    answer({ ok: false });
    await expect(getVerdicts(["a"])).rejects.toThrow();
    answer({ ok: true, items: { a: { ok: true, state: "approved" } } });
    await expect(getVerdicts(["a"])).rejects.toThrow();
  });

  it("ignores a card it did not ask about and refuses more than the cap", async () => {
    answer({ ok: true, items: { zzz: { ok: true, state: "passed", stale: false } } });
    expect((await getVerdicts(["a"])).size).toBe(0);
    await expect(getVerdicts(Array.from({ length: MAX_VERDICT_BATCH + 1 }, (_, n) => `i${n}`))).rejects.toThrow();
  });
});
