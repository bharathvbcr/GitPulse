import { describe, expect, it, vi } from "vitest";
import {
  MAX_WAVE_SIZE, clampWaveSize, clampWaves, planBatch, resumeWave, scopesOverlap, startWave,
  type Candidate, type RunRead, type Suggestion, type VerdictRead, type WaveDeps, type WaveRecord,
} from "./waveLaunch";

const task = (id: string): Candidate => ({ id, title: `Task ${id}` });
const passed = (head = "h1"): VerdictRead => ({ state: "passed", stale: false, head });

/** A fake sidecar: runs, verdicts, merges and the persisted record, all in memory. */
function world(options: { candidates?: Candidate[][]; scopes?: Record<string, string[]>; verdict?: (id: string, call: number) => VerdictRead; mergeError?: (id: string) => string | null; launchError?: (id: string) => string | null; runs?: Map<string, RunRead>; coverage?: Partial<Pick<Suggestion, "unread" | "unchecked">> } = {}) {
  const batches = options.candidates ?? [[task("a"), task("b"), task("c")]];
  let suggestCalls = 0, tick = 0, ids = 0;
  const runs = options.runs ?? new Map<string, RunRead>();
  const launched: string[] = [], merged: string[] = [], verifies: string[] = [], persisted: WaveRecord[] = [];
  const heads = new Map<string, string>();
  const deps: WaveDeps = {
    suggest: async () => ({ candidates: batches[Math.min(suggestCalls++, batches.length - 1)] ?? [], unread: "", unchecked: 0, ...options.coverage }),
    scopes: async (list) => new Map(list.filter((id) => options.scopes?.[id]).map((id) => [id, options.scopes![id]!])),
    launch: async (taskId, { runId }) => {
      const refusal = options.launchError?.(taskId);
      if (refusal) throw new Error(refusal);
      launched.push(taskId);
      const run = { state: "exited", exitCode: 0, cwd: `/wt/${taskId}`, reason: "" };
      runs.set(runId, run);
      return run;
    },
    getRun: async (runId) => runs.get(runId) ?? null,
    verify: async (id) => { verifies.push(id); return (options.verdict ?? (() => passed()))(id, verifies.filter((v) => v === id).length); },
    headOf: async () => "h1",
    merge: async ({ taskId: id, head }) => {
      const error = options.mergeError?.(id);
      if (error) throw new Error(error);
      merged.push(id); heads.set(id, head);
      return `Merged ${id}`;
    },
    persist: async (record) => { persisted.push(structuredClone(record)); },
    now: () => (tick += 1000),
    sleep: async () => { await Promise.resolve(); },
    id: () => `id${(ids += 1)}`,
    pollMs: 1,
  };
  return { deps, runs, launched, merged, verifies, persisted, heads };
}

const phases = (record: WaveRecord) => Object.fromEntries(record.members.map((m) => [m.taskId, m.phase]));

describe("bounds", () => {
  it("clamps the wave size and the number of waves, whatever it is given", () => {
    for (const bad of [NaN, Infinity, -Infinity, undefined, null, "3", {}]) {
      expect(clampWaveSize(bad)).toBe(3);
      expect(clampWaves(bad)).toBe(3);
    }
    expect(clampWaveSize(0)).toBe(1);
    expect(clampWaveSize(-9)).toBe(1);
    expect(clampWaveSize(99)).toBe(MAX_WAVE_SIZE);
    expect(clampWaves(99)).toBe(5);
    expect(clampWaveSize(2.9)).toBe(2);
  });
});

describe("file-scope collisions", () => {
  it("overlaps equal paths and directory containment, not siblings or name prefixes", () => {
    expect(scopesOverlap(["src/a.ts"], ["./src/a.ts"])).toBe(true);
    expect(scopesOverlap(["src/lib"], ["src/lib/x.ts"])).toBe(true);
    expect(scopesOverlap(["src\\lib\\"], ["src/lib/x.ts"])).toBe(true);
    expect(scopesOverlap(["src/lib"], ["src/library/x.ts"])).toBe(false);
    expect(scopesOverlap(["a.ts"], ["b.ts"])).toBe(false);
    expect(scopesOverlap([], ["b.ts"])).toBe(false);
  });

  it("never admits two tasks with overlapping scopes into one batch, and skips the later by rank", () => {
    const scopes = new Map([["a", ["src/x.ts"]], ["b", ["src/x.ts", "src/y.ts"]], ["c", ["docs/z.md"]]]);
    const plan = planBatch([task("a"), task("b"), task("c")], scopes, 3, new Set());
    expect(plan.chosen.map((c) => c.id)).toEqual(["a", "c"]);
    expect(plan.skipped).toEqual([{ id: "b", title: "Task b", reason: 'its files overlap "Task a"' }]);
  });

  it("treats an unknown scope as colliding with everything, so it runs alone", () => {
    const scopes = new Map([["b", ["x"]], ["c", ["y"]]]);
    expect(planBatch([task("a"), task("b"), task("c")], scopes, 3, new Set()).chosen.map((c) => c.id)).toEqual(["a"]);
    expect(planBatch([task("b"), task("a"), task("c")], scopes, 3, new Set()).chosen.map((c) => c.id)).toEqual(["b", "c"]);
  });

  it("honours the bound, the exclusion list and duplicate suggestions", () => {
    const scopes = new Map([["a", ["1"]], ["b", ["2"]], ["c", ["3"]], ["d", ["4"]]]);
    expect(planBatch([task("a"), task("a"), task("b"), task("c"), task("d")], scopes, 2, new Set()).chosen.map((c) => c.id)).toEqual(["a", "b"]);
    expect(planBatch([task("a"), task("b")], scopes, 2, new Set(["a"])).chosen.map((c) => c.id)).toEqual(["b"]);
    expect(planBatch([], scopes, 2, new Set()).chosen).toEqual([]);
  });
});

describe("a wave end to end against a fake sidecar", () => {
  it("launches the top N, merges only passed work one at a time, and finishes", async () => {
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] } });
    const runner = await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps);
    const done = await runner.finished;
    expect(done.state).toBe("done");
    expect(w.launched).toEqual(["a", "b", "c"]);
    expect(w.merged).toEqual(["a", "b", "c"]);
    expect(phases(done)).toEqual({ a: "merged", b: "merged", c: "merged" });
    // Each merge was followed by a re-verify: a verify before and one after, per task.
    for (const id of ["a", "b", "c"]) expect(w.verifies.filter((v) => v === id)).toHaveLength(2);
    // The seq written is strictly increasing, so a resume can trust the highest.
    const seqs = w.persisted.map((r) => r.seq);
    expect(seqs).toEqual([...seqs].sort((x, y) => x - y));
    expect(new Set(seqs).size).toBe(seqs.length);
  });

  it("launches the next N only after the first batch has no live member, and never relaunches an attempted task", async () => {
    const w = world({ candidates: [[task("a"), task("b")], [task("a"), task("b"), task("c"), task("d")]], scopes: { a: ["a"], b: ["b"], c: ["c"], d: ["d"] } });
    const done = await (await startWave({ repositoryId: "r", n: 2, waves: 2 }, w.deps)).finished;
    expect(w.launched).toEqual(["a", "b", "c", "d"]);
    expect(done.batch).toBe(2);
    expect(done.state).toBe("done");
  });

  it("holds degraded, failed, could-not-run, none and stale verdicts and merges none of them", async () => {
    for (const state of ["degraded", "failed", "could_not_run", "none", "running"]) {
      const w = world({ candidates: [[task("a")]], verdict: () => ({ state, stale: false, head: "h1" }) });
      const done = await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps)).finished;
      expect(w.merged, state).toEqual([]);
      expect(phases(done), state).toEqual({ a: "held" });
      expect(done.members[0]!.detail, state).toMatch(/not a pass/);
    }
    const stale = world({ candidates: [[task("a")]], verdict: () => ({ state: "passed", stale: true, head: "h1" }) });
    expect((await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, stale.deps)).finished).members[0]!.phase).toBe("held");
    expect(stale.merged).toEqual([]);
    const other = world({ candidates: [[task("a")]], verdict: () => passed("not-the-head") });
    expect((await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, other.deps)).finished).members[0]!.detail).toMatch(/not for the commit/);
    expect(other.merged).toEqual([]);
  });

  it("holds a task whose verification throws, and carries on with the rest", async () => {
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] }, verdict: (id) => { if (id === "b") throw new Error("sidecar down"); return passed(); } });
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(phases(done)).toEqual({ a: "merged", b: "held", c: "merged" });
    expect(done.members[1]!.detail).toMatch(/could not run: sidecar down/);
  });

  it("shows a merge conflict on that task, never resolves it, and merges the others", async () => {
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] }, mergeError: (id) => (id === "b" ? "merge conflict in src/b.ts" : null) });
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(phases(done)).toEqual({ a: "merged", b: "held", c: "merged" });
    expect(done.members[1]!.detail).toMatch(/Not merged: merge conflict in src\/b\.ts/);
    expect(w.merged).toEqual(["a", "c"]);
  });

  it("halts further merges when a re-verify after a merge is not a pass", async () => {
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] }, verdict: (id, call) => (id === "a" && call === 2 ? { state: "degraded", stale: false, head: "h1" } : passed()) });
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(w.merged).toEqual(["a"]);
    expect(phases(done)).toEqual({ a: "merged", b: "stopped", c: "stopped" });
    expect(done.note).toMatch(/Re-verification after merging "Task a" was degraded/);
  });

  it("shows a refused launch and does not retry it or launch it again in a later batch", async () => {
    let attempts = 0;
    const w = world({ candidates: [[task("a"), task("b")], [task("a"), task("b")]], scopes: { a: ["a"], b: ["b"] }, launchError: (id) => { if (id === "a") { attempts += 1; return "the host is under load"; } return null; } });
    const done = await (await startWave({ repositoryId: "r", n: 2, waves: 2 }, w.deps)).finished;
    expect(attempts).toBe(1);
    expect(done.batch).toBe(1);
    expect(done.state).toBe("done");
    expect(done.members.find((m) => m.taskId === "a")!.phase).toBe("held");
    expect(done.attempted).toEqual(expect.arrayContaining(["a", "b"]));
    const first = w.persisted.find((r) => r.members.some((m) => m.taskId === "a" && m.phase === "held"));
    expect(first?.members.find((m) => m.taskId === "a")!.detail).toBe("Not launched: the host is under load");
  });

  it("holds a run that exited non-zero or was cancelled, without verifying it", async () => {
    const runs = new Map<string, RunRead>();
    const w = world({ candidates: [[task("a")]], runs });
    const original = w.deps.launch;
    w.deps.launch = async (id, ids, signal) => { await original(id, ids, signal); const bad = { state: "exited", exitCode: 2, cwd: "/wt/a", reason: "" }; runs.set(ids.runId, bad); return bad; };
    const done = await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps)).finished;
    expect(done.members[0]!.detail).toBe("The run exited with code 2.");
    expect(w.verifies).toEqual([]);
  });

  it("stops the wave when its state cannot be persisted, rather than carrying on unrecorded", async () => {
    const w = world({ candidates: [[task("a")]] });
    let writes = 0;
    w.deps.persist = async () => { writes += 1; if (writes > 2) throw new Error("disk full"); };
    const done = await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("failed");
    expect(done.note).toMatch(/disk full/);
    expect(w.merged).toEqual([]);
  });

  it("ends done with a note when nothing is ready to start", async () => {
    const w = world({ candidates: [[]] });
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 3 }, w.deps)).finished;
    expect(done.state).toBe("done");
    expect(done.note).toBe("No further task is ready to start.");
    expect(w.launched).toEqual([]);
  });
});

describe("cancel", () => {
  it("stops launching and merging, leaves running agents to finish, and says so", async () => {
    const w = world({ candidates: [[task("a"), task("b")], [task("c")]], scopes: { a: ["a"], b: ["b"], c: ["c"] } });
    const deps = { ...w.deps };
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    // The first run never ends, so the wave is waiting on it when cancelled.
    deps.getRun = async (runId) => { const run = w.runs.get(runId); return run ? { ...run, state: "running", exitCode: null } : null; };
    deps.sleep = async (_ms, signal) => { await Promise.race([gate, new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve()))]); };
    const runner = await startWave({ repositoryId: "r", n: 2, waves: 2 }, deps);
    while (w.launched.length < 2) await Promise.resolve();
    await runner.cancel();
    release();
    const done = await runner.finished;
    expect(done.state).toBe("cancelled");
    expect(w.launched).toEqual(["a", "b"]);
    expect(w.merged).toEqual([]);
    expect(Object.values(phases(done))).toEqual(["stopped", "stopped"]);
  });

  it("is idempotent and cannot revive a finished wave", async () => {
    const w = world({ candidates: [[task("a")]] });
    const runner = await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps);
    await runner.finished;
    await runner.cancel();
    await runner.cancel();
    expect(runner.record().state).toBe("done");
  });
});

describe("restart mid-wave", () => {
  it("adopts a run it had claimed instead of launching it again", async () => {
    const w = world({ scopes: { a: ["a"], b: ["b"] }, candidates: [[task("a"), task("b")]] });
    // Crash after the claim was persisted but before anything launched.
    let crashed = false;
    const first = { ...w.deps, persist: async (record: WaveRecord) => { await w.deps.persist(record); if (record.members.length && !crashed) { crashed = true; throw new Error("process died"); } } };
    await (await startWave({ repositoryId: "r", n: 2, waves: 1 }, first)).finished;
    const claimed = w.persisted.find((r) => r.members.length === 2)!;
    expect(claimed.members.map((m) => m.phase)).toEqual(["launching", "launching"]);
    // One run had actually been prepared before the crash.
    w.runs.set(claimed.members[0]!.runId, { state: "running", exitCode: null, cwd: "/wt/a", reason: "" });
    w.runs.get(claimed.members[0]!.runId)!.state = "exited"; w.runs.get(claimed.members[0]!.runId)!.exitCode = 0;
    const runner = await resumeWave(claimed, w.deps);
    const done = await runner.finished;
    // Task a was adopted (never launched again); b had no run, so it is prepared with its recorded ids.
    expect(w.launched).toEqual(["b"]);
    expect(done.members.map((m) => m.runId)).toEqual(claimed.members.map((m) => m.runId));
    expect(phases(done)).toEqual({ a: "merged", b: "merged" });
  });

  it("does not repeat a merge that was interrupted; it holds the task and says so", async () => {
    const w = world({ candidates: [[task("a")]] });
    const done0 = await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps)).finished;
    const interrupted: WaveRecord = { ...done0, state: "running", members: done0.members.map((m) => ({ ...m, phase: "merging" as const })) };
    const before = w.merged.length;
    const done = await (await resumeWave(interrupted, w.deps)).finished;
    expect(w.merged).toHaveLength(before);
    expect(done.members[0]!.phase).toBe("held");
    expect(done.members[0]!.detail).toMatch(/Interrupted while merging/);
  });

  it("returns a wave that is not running exactly as it was, doing nothing", async () => {
    const w = world();
    const record: WaveRecord = { v: 1, id: "x", repositoryId: "r", n: 3, batchesLeft: 2, batch: 1, state: "cancelled", members: [], attempted: [], note: "n", seq: 7 };
    const done = await (await resumeWave(record, w.deps)).finished;
    expect(done).toEqual(record);
    expect(w.persisted).toEqual([]);
    expect(w.launched).toEqual([]);
  });
});

/** Never settles on its own, like a 20-minute verify wait or a worktree preparation; ends only when the signal does. */
const untilAborted = (signal: AbortSignal) => new Promise<never>((_resolve, reject) => {
  if (signal.aborted) reject(new Error("aborted"));
  signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
});

describe("stop reaches every wait", () => {
  it("interrupts a verify wait at once and leaves the member stopped, not held for a verification that never ran", async () => {
    vi.useFakeTimers();
    try {
      const w = world({ candidates: [[task("a"), task("b")]], scopes: { a: ["a"], b: ["b"] } });
      w.deps.verify = (_id, signal) => untilAborted(signal);
      const runner = await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps);
      await vi.advanceTimersByTimeAsync(5_000);
      expect(runner.record().members.map((m) => m.phase)).toContain("verifying");
      let settled = false;
      const stopping = runner.cancel().then(() => { settled = true; });
      await vi.advanceTimersByTimeAsync(1_000);
      expect(settled).toBe(true);
      await stopping;
      expect(runner.record().state).toBe("cancelled");
      expect(Object.values(phases(runner.record()))).toEqual(["stopped", "stopped"]);
      expect(w.merged).toEqual([]);
    } finally { vi.useRealTimers(); }
  });

  it("interrupts a launch that is still preparing its worktree", async () => {
    vi.useFakeTimers();
    try {
      const w = world({ candidates: [[task("a")]] });
      w.deps.launch = (_id, _ids, signal) => untilAborted(signal);
      const runner = await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps);
      await vi.advanceTimersByTimeAsync(1_000);
      let settled = false;
      void runner.cancel().then(() => { settled = true; });
      await vi.advanceTimersByTimeAsync(1_000);
      expect(settled).toBe(true);
      expect(runner.record().state).toBe("cancelled");
      expect(phases(runner.record())).toEqual({ a: "stopped" });
    } finally { vi.useRealTimers(); }
  });

  it("interrupts the suggest read of the next batch", async () => {
    const w = world({ candidates: [[task("a")], [task("b")]], scopes: { a: ["a"], b: ["b"] } });
    let calls = 0;
    const read = w.deps.suggest;
    w.deps.suggest = (signal) => (++calls === 1 ? read(signal) : untilAborted(signal));
    const runner = await startWave({ repositoryId: "r", n: 1, waves: 2 }, w.deps);
    while (calls < 2) await new Promise((resolve) => setTimeout(resolve, 0));
    await runner.cancel();
    expect(runner.record().state).toBe("cancelled");
    expect(w.launched).toEqual(["a"]);
  });

  it("a stop during a merge lets the merge finish, then does not halt on the interrupted re-verify", async () => {
    const w = world({ candidates: [[task("a"), task("b")]], scopes: { a: ["a"], b: ["b"] } });
    let release!: () => void;
    const merging = new Promise<void>((resolve) => { release = resolve; });
    let inMerge = false;
    const merge = w.deps.merge;
    w.deps.merge = async (target) => { inMerge = true; await merging; return merge(target); };
    const verify = w.deps.verify;
    w.deps.verify = (id, signal) => (w.merged.includes(id) ? untilAborted(signal) : verify(id, signal));
    const runner = await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps);
    while (!inMerge) await new Promise((resolve) => setTimeout(resolve, 0));
    const stopping = runner.cancel();
    release();
    await stopping;
    const done = runner.record();
    expect(done.state).toBe("cancelled");
    expect(w.merged).toEqual(["a"]);
    expect(phases(done)).toEqual({ a: "merged", b: "stopped" });
  });

  it("a second stop while the first is settling is harmless", async () => {
    const w = world({ candidates: [[task("a")]] });
    w.deps.verify = (_id, signal) => untilAborted(signal);
    const runner = await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps);
    while (!runner.record().members.some((m) => m.phase === "verifying")) await new Promise((resolve) => setTimeout(resolve, 0));
    await Promise.all([runner.cancel(), runner.cancel()]);
    await runner.cancel();
    expect(runner.record().state).toBe("cancelled");
  });

  it("still holds a member whose verification throws when nobody stopped the wave", async () => {
    const w = world({ candidates: [[task("a")]], verdict: () => { throw new Error("sidecar down"); } });
    const done = await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps)).finished;
    expect(phases(done)).toEqual({ a: "held" });
  });
});

describe("a partial board", () => {
  it("refuses to launch from a ranking whose links were not read, and records why", async () => {
    const w = world({ coverage: { unread: "385 open tasks' links were not read, so a blocker may be among what was not read." } });
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1, acceptUnchecked: true }, w.deps)).finished;
    expect(done.state).toBe("refused");
    expect(done.note).toMatch(/Nothing was launched: 385 open tasks' links were not read/);
    expect(w.launched).toEqual([]);
    expect(w.persisted.at(-1)?.state).toBe("refused");
  });

  it("refuses an unchecked ranking unless the person allowed it, and then records the caveat on the wave", async () => {
    const refused = world({ coverage: { unchecked: 4 } });
    const no = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, refused.deps)).finished;
    expect(no.state).toBe("refused");
    expect(no.note).toMatch(/4 tasks were not checked/);
    expect(refused.launched).toEqual([]);

    const allowed = world({ coverage: { unchecked: 1 }, scopes: { a: ["a"], b: ["b"], c: ["c"] } });
    const yes = await (await startWave({ repositoryId: "r", n: 3, waves: 1, acceptUnchecked: true }, allowed.deps)).finished;
    expect(yes.state).toBe("done");
    expect(allowed.launched).toEqual(["a", "b", "c"]);
    expect(yes.note).toMatch(/Ranked without a verification check on 1 task \(allowed/);
  });

  it("checks every batch, so a board that turns partial after the first batch stops the next", async () => {
    const w = world({ candidates: [[task("a")], [task("b")]], scopes: { a: ["a"], b: ["b"] } });
    let calls = 0;
    const read = w.deps.suggest;
    w.deps.suggest = async (signal) => ({ ...(await read(signal)), unread: ++calls > 1 ? "the board was only partly read." : "" });
    const done = await (await startWave({ repositoryId: "r", n: 1, waves: 2 }, w.deps)).finished;
    expect(done.state).toBe("refused");
    expect(w.launched).toEqual(["a"]);
    expect(phases(done)).toEqual({ a: "merged" });
  });
});
