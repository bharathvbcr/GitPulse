import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
const answer = (value: unknown) => invoke.mockResolvedValue(JSON.stringify(value));
const sent = () => invoke.mock.calls.map(([, args]) => JSON.parse((args as { input: string }).input));

import { MAX_VERDICT_BATCH, MAX_VERDICT_READS, getBoardVerdicts, getVerdicts, sameVerdicts, type CardVerdict } from "./verification";

describe("sameVerdicts", () => {
  const v = (state: CardVerdict["state"], stale = false): CardVerdict => ({ state, stale });
  it("matches reads that say the same about every card, and nothing else", () => {
    const shown = new Map([["a", v("passed")], ["b", v("failed")]]);
    expect(sameVerdicts(shown, new Map([["b", v("failed")], ["a", v("passed")]]))).toBe(true);
    expect(sameVerdicts(shown, new Map([["a", v("passed", true)], ["b", v("failed")]]))).toBe(false);
    expect(sameVerdicts(shown, new Map([["a", v("passed")], ["b", v("degraded")]]))).toBe(false);
    expect(sameVerdicts(shown, new Map([["a", v("passed")]]))).toBe(false);
    expect(sameVerdicts(shown, new Map([["a", v("passed")], ["c", v("failed")]]))).toBe(false);
  });
});

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

describe("getBoardVerdicts", () => {
  it("splits a board larger than one read into reads the host accepts, one after another", async () => {
    invoke.mockImplementation(async (_cmd: string, args?: { input: string }) => {
      const { ids } = JSON.parse(args?.input ?? "{\"ids\":[]}") as { ids: string[] };
      return JSON.stringify({ ok: true, items: Object.fromEntries(ids.filter((id) => id.endsWith("7")).map((id) => [id, { ok: true, state: "passed", stale: false }])) });
    });
    const ids = Array.from({ length: MAX_VERDICT_BATCH * 2 + 10 }, (_, n) => `i${n}`);
    const out = await getBoardVerdicts([...ids, ...ids.slice(0, 5)]);
    expect(sent().map((input) => input.ids.length)).toEqual([MAX_VERDICT_BATCH, MAX_VERDICT_BATCH, 10]);
    expect(sent().flatMap((input) => input.ids)).toEqual(ids);
    expect([...out.keys()]).toEqual(ids.filter((id) => id.endsWith("7")));
  });

  it("is one read for a board that fits, none for an empty one, and refuses past its bound", async () => {
    answer({ ok: true, items: {} });
    await getBoardVerdicts(["a", "b"]);
    expect(invoke).toHaveBeenCalledTimes(1);
    invoke.mockClear();
    expect((await getBoardVerdicts([])).size).toBe(0);
    expect(invoke).not.toHaveBeenCalled();
    await expect(getBoardVerdicts(Array.from({ length: MAX_VERDICT_BATCH * MAX_VERDICT_READS + 1 }, (_, n) => `i${n}`))).rejects.toThrow();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("fails the whole read when one part fails, rather than draw the rest as unverified", async () => {
    invoke.mockResolvedValueOnce(JSON.stringify({ ok: true, items: {} })).mockRejectedValueOnce({ code: "store_error", message: "store unavailable" });
    await expect(getBoardVerdicts(Array.from({ length: MAX_VERDICT_BATCH + 1 }, (_, n) => `i${n}`))).rejects.toThrow();
  });
});
