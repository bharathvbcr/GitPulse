import { describe, expect, it, vi } from "vitest";
import { MergeTeardownFailure } from "../branches/mergeTeardown";
import type { MergeTeardownError } from "../branches/types";
import {
  MAX_WAVE_SIZE, clampWaveSize, clampWaves, planBatch, resumeWave, scopesOverlap, startWave,
  type Candidate, type MainState, type MergeRead, type PostMerge, type RunRead, type Suggestion, type VerdictRead, type WaveDeps, type WaveRecord,
} from "./waveLaunch";

const task = (id: string): Candidate => ({ id, title: `Task ${id}` });
const passed = (head = "h1"): VerdictRead => ({ state: "passed", stale: false, head });

/** A fake sidecar: runs, verdicts, merges and the persisted record, all in memory. */
function world(options: { candidates?: Candidate[][]; scopes?: Record<string, string[]>; verdict?: (id: string, call: number, post?: PostMerge) => VerdictRead; mergeRead?: (id: string, call: number, said: string) => MergeRead; mergeError?: (id: string) => string | null; mergeFailure?: (id: string) => MergeTeardownError | null; mergeSaid?: (id: string) => string | null; main?: () => MainState; launchError?: (id: string) => string | null; runs?: Map<string, RunRead>; coverage?: Partial<Pick<Suggestion, "unread" | "unchecked">> } = {}) {
  const batches = options.candidates ?? [[task("a"), task("b"), task("c")]];
  let suggestCalls = 0, tick = 0, ids = 0, mainReads = 0;
  const runs = options.runs ?? new Map<string, RunRead>();
  const launched: string[] = [], merged: string[] = [], verifies: string[] = [], persisted: WaveRecord[] = [], posts: { id: string; post: PostMerge }[] = [];
  // The target's tip moves with every merge: t0, then t1, t2, ...
  let tip = "t0";
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
    verify: async (id, _signal, post) => {
      verifies.push(id);
      if (post) posts.push({ id, post });
      const verdict = (options.verdict ?? (() => passed()))(id, verifies.filter((v) => v === id).length, post);
      // A verdict for the merged checkout is for the tip asked about, whatever head the fake's default carries.
      return post && verdict.state === "passed" && verdict.head === "h1" ? { ...verdict, head: post.head } : verdict;
    },
    headOf: async () => "h1",
    merge: async ({ taskId: id, head }) => {
      const error = options.mergeError?.(id);
      if (error) throw new Error(error);
      const failure = options.mergeFailure?.(id);
      if (failure) throw new MergeTeardownFailure(failure);
      merged.push(id); heads.set(id, head);
      const said = options.mergeSaid?.(id) ?? `Merged ${id}`;
      const read: MergeRead = { message: said, beforeOid: tip, afterOid: `t${merged.length}` };
      tip = `t${merged.length}`;
      return options.mergeRead?.(id, merged.length, said) ?? read;
    },
    mainState: async () => { mainReads += 1; return options.main?.() ?? { dirty: 0, operation: null }; },
    persist: async (record) => { persisted.push(structuredClone(record)); },
    now: () => (tick += 1000),
    sleep: async () => { await Promise.resolve(); },
    id: () => `id${(ids += 1)}`,
    pollMs: 1,
    verifyLimit: 2,
  };
  return { deps, runs, launched, merged, verifies, persisted, heads, posts, mainReads: () => mainReads };
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
    // One verify before the merge, then the gate after each merge re-verifies every member merged so far: a is judged after all three merges, c after its own only.
    expect(w.verifies.filter((v) => v === "a")).toHaveLength(1 + 3);
    expect(w.verifies.filter((v) => v === "b")).toHaveLength(1 + 2);
    expect(w.verifies.filter((v) => v === "c")).toHaveLength(1 + 1);
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

  // This used to assert that c merged after b conflicted. A conflicting merge
  // is rolled back, but c was cut from the same base b conflicted with, and a
  // wave that carries on past a conflict is a wave that does not stop at the
  // first thing needing a person ("a conflict stops that task and is shown").
  it("halts the wave on a merge conflict: that task shows it, the ready ones are held pending it, and no batch follows", async () => {
    const conflict: MergeTeardownError = { code: "conflict", message: "Merging gitpulse/b into main conflicted in 1 file(s): src/b.ts. It was rolled back; nothing was merged and the worktree and the branch were kept.", paths: ["src/b.ts"], total: 1, rolled_back: true, rollback_error: null };
    const w = world({ candidates: [[task("a"), task("b"), task("c")], [task("d")]], scopes: { a: ["a"], b: ["b"], c: ["c"], d: ["d"] }, mergeFailure: (id) => (id === "b" ? conflict : null) });
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 2 }, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(phases(done)).toEqual({ a: "merged", b: "held", c: "held" });
    expect(done.members[1]!.detail).toMatch(/Not merged: Merging gitpulse\/b into main conflicted in 1 file\(s\): src\/b\.ts/);
    expect(done.members[2]!.detail).toBe("not merged: conflict in Task b pending");
    expect(w.merged).toEqual(["a"]);
    expect(w.launched).toEqual(["a", "b", "c"]);
    expect(done.batch).toBe(1);
    expect(done.note).toMatch(/conflict/);
  });

  it("holds every member not yet ready, as stopped, when a conflict halts the wave", async () => {
    const conflict: MergeTeardownError = { code: "conflict", message: "conflicted", paths: ["f"], total: 1, rolled_back: true, rollback_error: null };
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] }, mergeFailure: () => conflict, verdict: (id) => { if (id !== "a") throw new Error("sidecar down"); return passed(); } });
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(w.merged).toEqual([]);
    expect(done.members[0]!.phase).toBe("held");
    for (const m of done.members.slice(1)) expect(["held", "stopped"]).toContain(m.phase);
  });

  it("treats a parked squash like a conflict: reported as parked in main, and nothing merges after it", async () => {
    const parked: MergeTeardownError = { code: "squash_parked", message: "The squashed changes are staged but could not be committed", };
    const w = world({ scopes: { a: ["a"], b: ["b"] }, mergeFailure: (id) => (id === "a" ? parked : null) });
    const done = await (await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(done.members[1]!.detail).toBe("not merged: squash parked in main (Task a) pending");
    expect(w.merged).toEqual([]);
  });

  it("holds only that task on any other refusal, and merges the rest", async () => {
    for (const refusal of [{ code: "moved", message: "feature moved", branch: "b", expected: "e", actual: "a" }, { code: "refused", message: "nope" }] as MergeTeardownError[]) {
      const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] }, mergeFailure: (id) => (id === "b" ? refusal : null) });
      const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
      expect(phases(done)).toEqual({ a: "merged", b: "held", c: "merged" });
      expect(w.merged).toEqual(["a", "c"]);
    }
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] }, mergeError: (id) => (id === "b" ? "merge conflict in src/b.ts" : null) });
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(phases(done)).toEqual({ a: "merged", b: "held", c: "merged" });
    expect(done.members[1]!.detail).toMatch(/Not merged: merge conflict in src\/b\.ts/);
  });

  it("counts a merge whose cleanup is pending as merged, and still re-verifies it", async () => {
    const said = "Merged 1 commit from gitpulse/a into main; cleanup pending: the worktree could not be removed after 3 attempts.";
    const w = world({ candidates: [[task("a")]], mergeSaid: () => said });
    const done = await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps)).finished;
    expect(phases(done)).toEqual({ a: "merged" });
    expect(done.members[0]!.detail).toBe(said);
    expect(w.verifies.filter((v) => v === "a")).toHaveLength(2);
  });

  it("launches no batch while the main checkout is dirty or has an operation in progress, and says why", async () => {
    for (const [main, why] of [
      [{ dirty: 3, operation: null }, /3 uncommitted files/],
      [{ dirty: 0, operation: "squash merge" }, /squash merge is in progress/],
      [{ dirty: null, operation: null }, /could not be counted/],
    ] as [MainState, RegExp][]) {
      const w = world({ main: () => main });
      const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
      expect(done.state).toBe("refused");
      expect(done.note).toMatch(why);
      expect(w.launched).toEqual([]);
    }
    const unreadable = world();
    unreadable.deps.mainState = async () => { throw new Error("git is busy"); };
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, unreadable.deps)).finished;
    expect(done.state).toBe("refused");
    expect(done.note).toMatch(/could not be read \(git is busy\)/);
    expect(unreadable.launched).toEqual([]);
  });

  it("refuses each merge while the main checkout is dirty or mid-operation, merging nothing", async () => {
    let reads = 0;
    // Clean for the launch of the batch, then dirty for the merges.
    const w = world({ scopes: { a: ["a"], b: ["b"] }, main: () => ((reads += 1) === 1 ? { dirty: 0, operation: null } : { dirty: 2, operation: null }) });
    const done = await (await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps)).finished;
    expect(w.merged).toEqual([]);
    expect(phases(done)).toEqual({ a: "held", b: "held" });
    expect(done.members[0]!.detail).toMatch(/Not merged: the main checkout has 2 uncommitted files/);
    expect(w.mainReads()).toBeGreaterThan(1);
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
    w.deps.verify = (id, signal, post) => (w.merged.includes(id) ? untilAborted(signal) : verify(id, signal, post));
    const runner = await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps);
    while (!inMerge) await new Promise((resolve) => setTimeout(resolve, 0));
    const stopping = runner.cancel();
    release();
    await stopping;
    const done = runner.record();
    expect(done.state).toBe("cancelled");
    expect(w.merged).toEqual(["a"]);
    // The merge landed, but its gate was cut off: it is held with that said, never `merged` without a gate outcome.
    expect(phases(done)).toEqual({ a: "held", b: "stopped" });
    expect(done.members[0]!.detail).toMatch(/stopped before the merge was re-verified/);
  });

  it("interrupts a run read, a scope read and a head read that never answer", async () => {
    for (const hang of ["getRun", "scopes", "headOf"] as const) {
      vi.useFakeTimers();
      try {
        const w = world({ candidates: [[task("a")]] });
        if (hang === "getRun") w.deps.getRun = (_id: string, signal: AbortSignal) => untilAborted(signal);
        if (hang === "scopes") w.deps.scopes = (_ids: readonly string[], signal: AbortSignal) => untilAborted(signal);
        if (hang === "headOf") w.deps.headOf = (_cwd: string, signal: AbortSignal) => untilAborted(signal);
        const runner = await startWave({ repositoryId: "r", n: 1, waves: 1, acceptUnchecked: true }, w.deps);
        await vi.advanceTimersByTimeAsync(1_000);
        let settled = false;
        void runner.cancel().then(() => { settled = true; });
        await vi.advanceTimersByTimeAsync(1_000);
        expect(settled, hang).toBe(true);
        expect(runner.record().state, hang).toBe("cancelled");
        expect(w.merged, hang).toEqual([]);
      } finally { vi.useRealTimers(); }
    }
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
  it("keeps the person's allowance of an unchecked ranking across a restart", async () => {
    const w = world({ coverage: { unchecked: 2 }, candidates: [[task("a")], [task("b")]], scopes: { a: ["a"], b: ["b"] } });
    let crashed = false;
    const first = { ...w.deps, persist: async (record: WaveRecord) => { await w.deps.persist(record); if (record.members.some((m) => m.phase === "running") && !crashed) { crashed = true; throw new Error("process died"); } } };
    await (await startWave({ repositoryId: "r", n: 1, waves: 2, acceptUnchecked: true }, first)).finished;
    const saved = w.persisted.findLast((r) => r.state === "running")!;
    expect(saved.acceptUnchecked).toBe(true);
    // The restart reads the record back from the card, so the allowance must survive the codec too.
    const done = await (await resumeWave(structuredClone(saved), w.deps)).finished;
    expect(done.state).toBe("done");
    expect(w.launched).toEqual(["a", "b"]);
  });

  it("a resumed wave the person never allowed still refuses an unchecked ranking", async () => {
    const w = world({ candidates: [[task("a")], [task("b")]], scopes: { a: ["a"], b: ["b"] } });
    let crashed = false;
    const first = { ...w.deps, persist: async (record: WaveRecord) => { await w.deps.persist(record); if (record.members.some((m) => m.phase === "running") && !crashed) { crashed = true; throw new Error("process died"); } } };
    await (await startWave({ repositoryId: "r", n: 1, waves: 2 }, first)).finished;
    const saved = w.persisted.findLast((r) => r.state === "running")!;
    const read = w.deps.suggest;
    w.deps.suggest = async (signal) => ({ ...(await read(signal)), unchecked: 3 });
    const done = await (await resumeWave(saved, w.deps)).finished;
    expect(done.state).toBe("refused");
    expect(w.launched).toEqual(["a"]);
  });

  it("a stop pressed before the first suggest read returns launches nothing", async () => {
    const w = world();
    w.deps.suggest = (signal) => untilAborted(signal);
    const runner = await startWave({ repositoryId: "r", n: 3, waves: 1, acceptUnchecked: true }, w.deps);
    await runner.cancel();
    expect(runner.record().state).toBe("cancelled");
    expect(w.launched).toEqual([]);
  });

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

describe("adversarial: merge halting and main state", () => {
  const conflict: MergeTeardownError = { code: "conflict", message: "conflicted", paths: ["f"], total: 1, rolled_back: true, rollback_error: null };

  it("adv: a mainState that rejects before a merge holds the member and merges nothing", async () => {
    const w = world({ candidates: [[task("a")]] });
    let calls = 0;
    w.deps.mainState = async () => { if (calls++ >= 1) throw new Error("ipc down"); return { dirty: 0, operation: null }; };
    const done = await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps)).finished;
    expect(w.merged).toEqual([]);
    expect(done.members[0]!.phase).toBe("held");
    expect(done.members[0]!.detail).toMatch(/could not be read \(ipc down\)/);
  });

  it("adv: a mainState that rejects at launch refuses the wave with nothing launched", async () => {
    const w = world();
    w.deps.mainState = async () => { throw new Error("ipc down"); };
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("refused");
    expect(w.launched).toEqual([]);
  });

  it("adv: a mainState that throws a non-Error value is still a refusal, not a crash", async () => {
    const w = world();
    w.deps.mainState = async () => { throw { weird: true }; };
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("refused");
    expect(w.launched).toEqual([]);
  });

  it("adv: dirty null or a malformed mainState reading is not free", async () => {
    for (const bad of [{ dirty: null, operation: null }, { dirty: undefined, operation: null }, { dirty: NaN, operation: null }, { dirty: -1, operation: null }, {}, null] as unknown as MainState[]) {
      const w = world();
      w.deps.mainState = async () => bad;
      const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
      expect(w.launched, JSON.stringify(bad)).toEqual([]);
      expect(done.state, JSON.stringify(bad)).toBe("refused");
    }
  });

  it("adv: two members conflicting in one batch produce one halt and exactly one merge attempt", async () => {
    let attempts = 0;
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] }, mergeFailure: () => { attempts += 1; return conflict; } });
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 2 }, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(attempts).toBe(1);
    expect(w.merged).toEqual([]);
  });

  it("adv: a conflict whose typed object lost its paths still halts the wave", async () => {
    // What mergeAndTeardown throws when readMergeTeardownError cannot read the
    // object: the raw rejection. The host said conflict; the wave must stop.
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] } });
    w.deps.merge = async ({ taskId }) => {
      if (taskId === "b") throw { code: "conflict", message: "Merging b conflicted in 1 file(s)", total: 1, rolled_back: true, rollback_error: null };
      w.merged.push(taskId); return { message: `Merged ${taskId}`, beforeOid: `t${w.merged.length - 1}`, afterOid: `t${w.merged.length}` };
    };
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(done.state, JSON.stringify(done.members.map((m) => [m.taskId, m.phase, m.detail]))).toBe("halted");
    expect(w.merged).toEqual(["a"]);
  });

  it("adv: a wave resumed while main is parked merges nothing", async () => {
    const first = world({ candidates: [[task("a"), task("b")]], scopes: { a: ["a"], b: ["b"] } });
    let reads = 0;
    first.deps.mainState = async () => (reads++ === 0 ? { dirty: 0, operation: null } : { dirty: 0, operation: "squash merge" });
    await (await startWave({ repositoryId: "r", n: 2, waves: 1 }, first.deps)).finished;
    const ready = first.persisted.find((r) => r.members.some((m) => m.phase === "ready"));
    expect(ready).toBeTruthy();
    const second = world({ candidates: [[task("a"), task("b")]] });
    second.deps.mainState = async () => ({ dirty: 0, operation: "merge" });
    const done = await (await resumeWave(structuredClone(ready!), second.deps)).finished;
    expect(second.merged).toEqual([]);
    expect(done.members.every((m) => m.phase !== "merged")).toBe(true);
  });

  it("adv: a wave resumed after a reload mid-merge does not merge the member behind the interrupted one", async () => {
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] } });
    const member = (id: string, phase: WaveRecord["members"][number]["phase"]) => ({ taskId: id, title: `Task ${id}`, phase, runId: `run-${id}`, requestId: `req-${id}`, cwd: `/wt/${id}`, head: "h1", detail: "", startedAt: 1 });
    const record: WaveRecord = { v: 1, id: "w", repositoryId: "r", n: 3, batchesLeft: 0, batch: 1, state: "running", members: [member("a", "merged"), member("b", "merging"), member("c", "ready")], attempted: ["a", "b", "c"], note: "", seq: 5, acceptUnchecked: false };
    const done = await (await resumeWave(record, w.deps)).finished;
    expect(done.members[1]!.phase).toBe("held");
    // b's merge may have conflicted and been rolled back before the reload;
    // c was cut from the same base.
    expect(w.merged, "c merged behind a member whose merge outcome is unknown").toEqual([]);
  });

  it("adv: a cleanup-pending merge is a merge: the member is merged and re-verified", async () => {
    const w = world({ candidates: [[task("a")]], mergeSaid: () => "Merged 1 commit from x into main; cleanup pending: locked." });
    const done = await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps)).finished;
    expect(phases(done)).toEqual({ a: "merged" });
    expect(w.verifies.filter((v) => v === "a").length).toBeGreaterThanOrEqual(2);
  });
});

describe("the gate after a merge", () => {
  it("judges the checkout over the range from the pre-merge tip, and re-verifies the whole merged set", async () => {
    const w = world({ candidates: [[task("a"), task("b")]], scopes: { a: ["a"], b: ["b"] } });
    const done = await (await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("done");
    expect(phases(done)).toEqual({ a: "merged", b: "merged" });
    // After a: a over t0..t1. After b: a over t0..t2 (its own pre-merge tip, the tip now) and b over t1..t2.
    expect(w.posts).toEqual([
      { id: "a", post: { base: "t0", head: "t1" } },
      { id: "a", post: { base: "t0", head: "t2" } },
      { id: "b", post: { base: "t1", head: "t2" } },
    ]);
    expect(done.members.map((m) => [m.mergeBefore, m.mergeAfter])).toEqual([["t0", "t1"], ["t1", "t2"]]);
  });

  it("halts the wave when the second merge breaks a criterion of the first task", async () => {
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] }, verdict: (id, _call, post) => (id === "a" && post?.head === "t2" ? { state: "failed", stale: false, head: post.head } : passed()) });
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(w.merged).toEqual(["a", "b"]);
    expect(phases(done)).toEqual({ a: "merged", b: "merged", c: "stopped" });
    expect(done.note).toMatch(/Re-verification after merging "Task b" found "Task a" was failed/);
    expect(done.members[0]!.detail).toMatch(/Re-verification after merging "Task b" was failed/);
  });

  it("does not take an older verdict for the merged checkout's tip", async () => {
    const w = world({ candidates: [[task("a")]], verdict: (_id, _call, post) => (post ? { state: "passed", stale: false, head: "an-older-tip" } : passed()) });
    const done = await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(done.note).toMatch(/was not a verdict for the merged checkout's tip/);
  });

  it("fails closed when the merge did not say where it landed: held, halted, never verified", async () => {
    const w = world({ scopes: { a: ["a"], b: ["b"] }, mergeRead: (_id, _n, said) => ({ message: said, beforeOid: "t0", afterOid: null }) });
    const done = await (await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(w.merged).toEqual(["a"]);
    expect(phases(done)).toEqual({ a: "held", b: "stopped" });
    expect(done.members[0]!.detail).toMatch(/Where the merge landed was not recorded/);
    expect(w.posts).toEqual([]);
  });

  it("holds a member whose worktree was removed by hand and carries on with the rest", async () => {
    const w = world({ scopes: { a: ["a"], b: ["b"] }, mergeError: (id) => (id === "a" ? "/wt/a is no longer a worktree of /repo. It may already have been merged or removed." : null) });
    const done = await (await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps)).finished;
    expect(phases(done)).toEqual({ a: "held", b: "merged" });
    expect(done.members[0]!.detail).toMatch(/no longer a worktree/);
  });

  it("never writes a member as merged without having written it as verifying_merge, with where it landed, first", async () => {
    const w = world({ scopes: { a: ["a"], b: ["b"], c: ["c"] } });
    await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    const before = new Map<string, string>();
    for (const record of w.persisted) {
      for (const m of record.members) {
        if (m.phase === "merged" && before.get(m.taskId) !== "merged") expect(before.get(m.taskId), m.taskId).toBe("verifying_merge");
        if (m.phase === "verifying_merge") { expect(m.mergeBefore).toBeTruthy(); expect(m.mergeAfter).toBeTruthy(); }
        before.set(m.taskId, m.phase);
      }
    }
    expect([...before.values()]).toEqual(["merged", "merged", "merged"]);
  });

  it("a restart between the merge and its re-verify resumes in verifying_merge and runs the gate, without merging again", async () => {
    const w = world({ candidates: [[task("a")]] });
    let crashed = false;
    const first = { ...w.deps, persist: async (record: WaveRecord) => { await w.deps.persist(record); if (record.members.some((m) => m.phase === "verifying_merge") && !crashed) { crashed = true; throw new Error("process died"); } } };
    await (await startWave({ repositoryId: "r", n: 1, waves: 1 }, first)).finished;
    const saved = w.persisted.findLast((r) => r.state === "running")!;
    expect(saved.members[0]!.phase).toBe("verifying_merge");
    expect(w.posts).toEqual([]);
    const done = await (await resumeWave(structuredClone(saved), w.deps)).finished;
    expect(w.merged).toEqual(["a"]);
    expect(w.posts).toEqual([{ id: "a", post: { base: "t0", head: "t1" } }]);
    expect(done.state).toBe("done");
    expect(phases(done)).toEqual({ a: "merged" });
  });

  it("a resume in verifying_merge whose before-oid is missing halts with a reason and never calls it verified", async () => {
    const w = world({ candidates: [[task("a")]] });
    const member = { taskId: "a", title: "Task a", phase: "verifying_merge" as const, runId: "run-a", requestId: "req-a", cwd: "/wt/a", head: "h1", detail: "Merged a", startedAt: 1, mergeAfter: "t1" };
    const record: WaveRecord = { v: 1, id: "w", repositoryId: "r", n: 1, batchesLeft: 0, batch: 1, state: "running", members: [member], attempted: ["a"], note: "", seq: 5 };
    const done = await (await resumeWave(record, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(phases(done)).toEqual({ a: "held" });
    expect(done.note).toMatch(/where the merge landed was not recorded/);
    expect(w.verifies).toEqual([]);
  });
});

describe("verifying members together", () => {
  it("4 members at host limit 2 take about 2T, not 4T, and a ready member merges while others verify", async () => {
    vi.useFakeTimers();
    try {
      const T = 60_000;
      const w = world({ candidates: [[task("a"), task("b"), task("c"), task("d")]], scopes: { a: ["a"], b: ["b"], c: ["c"], d: ["d"] } });
      const finishedAt = new Map<string, number>(), mergedAt = new Map<string, number>(), verifying: string[] = [];
      const wait = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
      w.deps.sleep = (ms, signal) => new Promise<void>((resolve) => { const timer = setTimeout(resolve, ms); signal.addEventListener("abort", () => { clearTimeout(timer); resolve(); }, { once: true }); });
      w.deps.pollMs = 1_000;
      // Task a is quick; the others take T. Only the first verification takes time: the gate after a merge answers at once.
      w.deps.verify = async (id, _signal, post) => {
        if (post) return { state: "passed", stale: false, head: post.head };
        verifying.push(id);
        await wait(id === "a" ? T / 10 : T);
        finishedAt.set(id, Date.now());
        return passed();
      };
      const merge = w.deps.merge;
      w.deps.merge = async (target) => { mergedAt.set(target.taskId, Date.now()); return merge(target); };
      const start = Date.now();
      const runner = await startWave({ repositoryId: "r", n: 4, waves: 1 }, w.deps);
      let end = 0;
      void runner.finished.then(() => { end = Date.now(); });
      await vi.advanceTimersByTimeAsync(5 * T);
      expect(runner.record().state).toBe("done");
      expect(w.merged).toHaveLength(4);
      // Never more than two verifications at once: the third starts only when a slot frees.
      expect(verifying.slice(0, 2)).toEqual(["a", "b"]);
      expect(end - start).toBeGreaterThanOrEqual(2 * T);
      expect(end - start).toBeLessThan(3 * T);
      // a merged while b, c and d were still verifying.
      expect(mergedAt.get("a")!).toBeLessThan(finishedAt.get("b")!);
    } finally { vi.useRealTimers(); }
  });

  it("persists verifying, so a reload finds the members mid-verification", async () => {
    const w = world({ candidates: [[task("a"), task("b")]], scopes: { a: ["a"], b: ["b"] } });
    expect(w.persisted.length).toBe(0);
    await (await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps)).finished;
    const mid = w.persisted.find((r) => r.members.length === 2 && r.members.every((m) => m.phase === "verifying"));
    expect(mid).toBeTruthy();
    // Resumed from there, the verifications are started again and the wave finishes.
    const again = world({ candidates: [[task("a"), task("b")]], scopes: { a: ["a"], b: ["b"] } });
    const done = await (await resumeWave(structuredClone(mid!), again.deps)).finished;
    expect(done.state).toBe("done");
    expect(again.merged).toEqual(["a", "b"]);
  });

  it("writes the record one write at a time, though verifications settle together", async () => {
    const w = world({ candidates: [[task("a"), task("b"), task("c")]], scopes: { a: ["a"], b: ["b"], c: ["c"] } });
    let writing = 0, overlapped = false;
    const persist = w.deps.persist;
    w.deps.persist = async (record) => { writing += 1; if (writing > 1) overlapped = true; await new Promise((resolve) => setTimeout(resolve, 0)); await persist(record); writing -= 1; };
    await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(overlapped).toBe(false);
  });
});

describe("stopping", () => {
  it("says it is stopping while a merge is in flight, and the merge is not cut off", async () => {
    const w = world({ candidates: [[task("a")]] });
    let release!: () => void;
    const held = new Promise<void>((resolve) => { release = resolve; });
    let inMerge = false;
    const merge = w.deps.merge;
    w.deps.merge = async (target) => { inMerge = true; await held; return merge(target); };
    const runner = await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps);
    while (!inMerge) await new Promise((resolve) => setTimeout(resolve, 0));
    expect(runner.stopping()).toBe(false);
    const stopping = runner.cancel();
    expect(runner.stopping()).toBe(true);
    release();
    await stopping;
    expect(runner.stopping()).toBe(false);
    expect(w.merged).toEqual(["a"]);
  });

  it("a stop during the gate leaves the merged member held and says so", async () => {
    const w = world({ candidates: [[task("a")]] });
    let inGate = false;
    const verify = w.deps.verify;
    w.deps.verify = (id, signal, post) => { if (!post) return verify(id, signal, post); inGate = true; return untilAborted(signal); };
    const runner = await startWave({ repositoryId: "r", n: 1, waves: 1 }, w.deps);
    while (!inGate) await new Promise((resolve) => setTimeout(resolve, 0));
    await runner.cancel();
    expect(runner.record().state).toBe("cancelled");
    expect(phases(runner.record())).toEqual({ a: "held" });
  });
});

describe("file scopes follow the filesystem's case rules", () => {
  it("treats src/A.ts and src/a.ts as overlapping on a case-insensitive filesystem, and as distinct on a case-sensitive one", () => {
    expect(scopesOverlap(["src/A.ts"], ["src/a.ts"], { caseInsensitive: true })).toBe(true);
    expect(scopesOverlap(["src/A.ts"], ["src/a.ts"], { caseInsensitive: false })).toBe(false);
    expect(scopesOverlap(["Src"], ["src/x.ts"], { caseInsensitive: true })).toBe(true);
    expect(scopesOverlap(["Src"], ["src/x.ts"], { caseInsensitive: false })).toBe(false);
    const scopes = new Map([["a", ["src/A.ts"]], ["b", ["src/a.ts"]]]);
    expect(planBatch([task("a"), task("b")], scopes, 2, new Set(), { caseInsensitive: true }).chosen.map((c) => c.id)).toEqual(["a"]);
    expect(planBatch([task("a"), task("b")], scopes, 2, new Set(), { caseInsensitive: false }).chosen.map((c) => c.id)).toEqual(["a", "b"]);
  });
});
