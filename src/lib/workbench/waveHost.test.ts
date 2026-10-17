import { describe, expect, it } from "vitest";
import { HOST_VERIFY_LIMIT, readSuggestion, suggestParams } from "./waveHost";
import { MAX_SUGGESTIONS, taskSuggestions } from "./client";
import { startWave, type WaveDeps } from "./waveLaunch";

describe("what a wave asks Manvi to rank", () => {
  it("carries the Suggest next panel's priority filter, and nothing when it is any priority", () => {
    expect(suggestParams("r1")).toEqual({ limit: MAX_SUGGESTIONS, repository_id: "r1" });
    expect(suggestParams("r1", 0)).toEqual({ limit: MAX_SUGGESTIONS, repository_id: "r1", max_priority: 0 });
    expect(suggestParams("r1", 2).max_priority).toBe(2);
  });

  it("keeps to the host's verification limit", () => {
    expect(HOST_VERIFY_LIMIT).toBe(2);
  });
});

describe("Stop reaches the host's waits", () => {
  it("a wave stopped while its verification waits is not left waiting for the verdict", async () => {
    // The host's verify polls under the wave's signal; this stands in for it with a wait that only the signal ends.
    const hang = (signal: AbortSignal) => new Promise<never>((_resolve, reject) => signal.addEventListener("abort", () => reject(new Error("The wave was stopped while waiting for verification.")), { once: true }));
    let persisted = 0;
    const deps: WaveDeps = {
      suggest: async () => ({ candidates: [{ id: "a", title: "A" }], unread: "", unchecked: 0 }),
      scopes: async () => new Map([["a", ["a.ts"]]]),
      launch: async () => ({ state: "exited", exitCode: 0, cwd: "/wt/a", reason: "" }),
      getRun: async () => ({ state: "exited", exitCode: 0, cwd: "/wt/a", reason: "" }),
      verify: (_id, signal) => hang(signal),
      headOf: async () => "h",
      merge: async () => ({ message: "merged", beforeOid: "t0", afterOid: "t1" }),
      mainState: async () => ({ dirty: 0, operation: null }),
      persist: async () => { persisted += 1; },
      now: () => 0, sleep: async () => { await Promise.resolve(); }, id: (() => { let n = 0; return () => `id${(n += 1)}`; })(), pollMs: 1, verifyLimit: HOST_VERIFY_LIMIT,
    };
    const runner = await startWave({ repositoryId: "r", n: 1, waves: 1 }, deps);
    while (!runner.record().members.some((m) => m.phase === "verifying")) await new Promise((resolve) => setTimeout(resolve, 0));
    await runner.cancel();
    expect(runner.record().state).toBe("cancelled");
    expect(runner.stopping()).toBe(false);
    expect(persisted).toBeGreaterThan(0);
  });
});

const answer = (over: Record<string, unknown> = {}) => taskSuggestions({
  ok: true, generated_at: 1, candidates: 2,
  suggestions: [
    { rank: 1, id: "a", title: "A", status: "inbox", priority: 0, score: 9, reason: "", signals: [] },
    { rank: 2, id: "b", title: "B", status: "inbox", priority: 1, score: 8, reason: "", signals: [] },
  ],
  blocked: [],
  excluded: { in_progress: 0, review: 0, duplicate: 0, blocked: 0, unread: 0, filtered: 0 },
  board: { scanned: 535, complete: true, unhydrated: 0 },
  verification: { checked: 535, unchecked: 0, failed: 0 },
  ...over,
});

describe("what a wave reads from a suggest answer", () => {
  it("passes a complete answer through with no caveat", () => {
    expect(readSuggestion(answer())).toEqual({ candidates: [{ id: "a", title: "A" }, { id: "b", title: "B" }], unread: "", unchecked: 0 });
  });

  it("keeps the partial-board facts the wave used to drop", () => {
    const read = readSuggestion(answer({ board: { scanned: 150, complete: false, unhydrated: 385 }, excluded: { in_progress: 0, review: 0, duplicate: 0, blocked: 0, unread: 3, filtered: 0 }, verification: { checked: 10, unchecked: 140, failed: 0 } }));
    expect(read.unread).toMatch(/only partly read \(150 tasks scanned\)/);
    expect(read.unread).toMatch(/385 open tasks' links were not read/);
    expect(read.unread).toMatch(/3 tasks were left out unread/);
    expect(read.unchecked).toBe(140);
  });

  it("never offers a task Manvi lists as blocked", () => {
    const read = readSuggestion(answer({ blocked: [{ id: "a", title: "A", blocked_by: ["z"] }] }));
    expect(read.candidates.map((c) => c.id)).toEqual(["b"]);
  });
});

describe("an answer missing the coverage fields", () => {
  it("is refused by the decoder, never read as a complete board", () => {
    const full = { ok: true, generated_at: 1, candidates: 0, suggestions: [], blocked: [],
      excluded: { in_progress: 0, review: 0, duplicate: 0, blocked: 0, unread: 0, filtered: 0 },
      board: { scanned: 1, complete: true, unhydrated: 0 }, verification: { checked: 1, unchecked: 0, failed: 0 } };
    for (const drop of [{ board: undefined }, { board: { scanned: 1, unhydrated: 0 } }, { board: { scanned: 1, complete: true } }, { verification: undefined }, { verification: { checked: 1, failed: 0 } }]) {
      expect(() => taskSuggestions({ ...full, ...drop }), JSON.stringify(drop)).toThrow();
    }
  });
});
