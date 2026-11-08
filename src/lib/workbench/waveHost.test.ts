import { describe, expect, it } from "vitest";
import { heldFromRisk, HOST_VERIFY_LIMIT, readSuggestion, suggestParams } from "./waveHost";
import { MAX_SUGGESTIONS, taskSuggestions } from "./client";
import { planBatch, startWave, type WaveDeps } from "./waveLaunch";

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

function waveDeps(over: Partial<WaveDeps> = {}): WaveDeps {
  return {
    suggest: async () => ({ candidates: [{ id: "a", title: "Task a" }, { id: "b", title: "Task b" }], unread: "", unchecked: 0 }),
    scopes: async () => new Map([["a", ["a.rs"]], ["b", ["b.rs"]]]),
    launch: async () => ({ state: "running", exitCode: null, cwd: "/wt", reason: "" }),
    getRun: async () => ({ state: "running", exitCode: null, cwd: "/wt", reason: "" }),
    verify: async () => ({ state: "passed", stale: false, head: "h" }),
    headOf: async () => "h",
    merge: async () => ({ message: "merged", beforeOid: "t0", afterOid: "t1" }),
    mainState: async () => ({ dirty: 0, operation: null }),
    persist: async () => {},
    now: () => 0,
    sleep: (ms, signal) => new Promise((resolve) => {
      if (signal.aborted) return resolve();
      const timer = setTimeout(resolve, ms);
      signal.addEventListener("abort", () => { clearTimeout(timer); resolve(); }, { once: true });
    }),
    id: (() => { let n = 0; return () => `id${(n += 1)}`; })(),
    pollMs: 1,
    verifyLimit: 1,
    ...over,
  };
}

describe("claims already recorded are told, never a hold", () => {
  it("names the claiming run in a notice and launches the member anyway", () => {
    const read = heldFromRisk({
      file_claims: [{ source: "declared", run_id: "X", path: "a.rs", worktree: "/other", ts_ms: 1 }],
    });
    expect(read.unreadable).toBeUndefined();
    const plan = planBatch(
      [{ id: "a", title: "Task a" }, { id: "b", title: "Task b" }],
      new Map([["a", ["a.rs"]], ["b", ["b.rs"]]]),
      2,
      new Set(),
      { caseInsensitive: false },
      read.held,
    );
    expect(plan.chosen.map((candidate) => candidate.id)).toEqual(["a", "b"]);
    expect(plan.skipped).toEqual([]);
    expect(plan.notices.map((notice) => notice.reason)).toEqual(["a.rs is claimed by run X"]);
  });

  it("plans the batch and says so in the note when the claim store was not read", async () => {
    const runner = await startWave({ repositoryId: "r", n: 2, waves: 1 }, waveDeps({
      heldPaths: async () => heldFromRisk({ claims_error: "cannot open claim lock" }),
      
    }));
    while (runner.record().members.length === 0) await new Promise((resolve) => setTimeout(resolve, 0));
    const { note, members } = runner.record();
    await runner.cancel();
    expect(members.map((member) => member.taskId)).toEqual(["a", "b"]);
    expect(note).toContain("cannot open claim lock");
    expect(note).toContain("none were checked");
  });

  it("notes a cross-batch claim in the wave record without holding the member back", async () => {
    const runner = await startWave({ repositoryId: "r", n: 2, waves: 1 }, waveDeps({
      heldPaths: async () => ({ held: [{ path: "a.rs", holder: "run X" }] }),
    }));
    while (runner.record().members.length === 0) await new Promise((resolve) => setTimeout(resolve, 0));
    const { note, members } = runner.record();
    await runner.cancel();
    expect(members.map((member) => member.taskId)).toEqual(["a", "b"]);
    expect(note).toContain("a.rs is claimed by run X");
    expect(note).not.toContain("Held back");
  });

  it("reads an observed claim as its session, an absent list as nothing held, and a holderless claim as unread", () => {
    expect(heldFromRisk({ file_claims: [{ source: "observed", session_id: "session-a", path: "a.rs", worktree: "/wt", ts_ms: 1 }] }))
      .toEqual({ held: [{ path: "a.rs", holder: "session session-a" }] });
    expect(heldFromRisk({})).toEqual({ held: [] });
    expect(heldFromRisk({ file_claims: [{ source: "declared", path: "a.rs", worktree: "/wt", ts_ms: 1 }] }).unreadable).toMatch(/named no holder/);
  });

  it("says a cut claim list was cut, never that nothing else is claimed", () => {
    const read = heldFromRisk({ file_claims: [{ source: "declared", run_id: "X", path: "a.rs", worktree: "/o", ts_ms: 1 }], claims_truncated: true, claims_total: 900 });
    expect(read.held).toHaveLength(1);
    expect(read.unreadable).toMatch(/only the newest 1 of 900 claims were returned/);
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
