/**
 * Adversarial review of the run-in-waves runner (task ft-007a2b4b). Each test
 * either pins a behaviour the review attacked and found sound, or documents a
 * break with `it.fails` (the body asserts the correct behaviour and currently
 * does not hold).
 */
import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import { HOST_VERIFY_LIMIT } from "./waveHost";
import { WAVE_LABEL_PREFIX, WAVE_MARK, decodeWave, encodeWave, findActiveWave, waveLabel, type WaveTaskApi } from "./waveStore";
import { planBatch, resumeWave, scopesOverlap, startWave, type Candidate, type PostMerge, type VerdictRead, type WaveDeps, type WaveRecord } from "./waveLaunch";

const task = (id: string): Candidate => ({ id, title: `Task ${id}` });
const passed = (head = "h1"): VerdictRead => ({ state: "passed", stale: false, head });
const phases = (record: WaveRecord) => Object.fromEntries(record.members.map((m) => [m.taskId, m.phase]));
const untilAborted = (signal: AbortSignal) => new Promise<never>((_resolve, reject) => {
  if (signal.aborted) { reject(new Error("aborted")); return; }
  signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
});
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

function world(ids: string[], over: Partial<WaveDeps> = {}) {
  const persisted: WaveRecord[] = [], merged: string[] = [], posts: { id: string; post: PostMerge }[] = [], firsts: string[] = [];
  let seq = 0, tip = 0, active = 0, peak = 0;
  const deps: WaveDeps = {
    suggest: async () => ({ candidates: ids.map(task), unread: "", unchecked: 0 }),
    scopes: async (list) => new Map(list.map((id) => [id, [`f-${id}`]])),
    launch: async (id) => ({ state: "exited", exitCode: 0, cwd: `/wt/${id}`, reason: "" }),
    getRun: async (_runId) => ({ state: "exited", exitCode: 0, cwd: "/wt/x", reason: "" }),
    verify: async (id, _signal, post) => {
      active += 1; peak = Math.max(peak, active);
      try {
        await tick();
        if (post) { posts.push({ id, post }); return passed(post.head); }
        firsts.push(id);
        return passed();
      } finally { active -= 1; }
    },
    headOf: async () => "h1",
    merge: async ({ taskId }) => { merged.push(taskId); const before = `t${tip}`; tip += 1; return { message: `Merged ${taskId}`, beforeOid: before, afterOid: `t${tip}` }; },
    mainState: async () => ({ dirty: 0, operation: null }),
    persist: async (record) => { persisted.push(structuredClone(record)); },
    now: () => 1_000,
    sleep: async () => { await tick(); },
    id: () => `id${(seq += 1)}`,
    pollMs: 1,
    verifyLimit: 2,
    ...over,
  };
  return { deps, persisted, merged, posts, firsts, peak: () => peak };
}

describe("merged-set re-verify is bounded", () => {
  it("5 members cost 5 first verifications plus 15 gate verifications, never more than the limit at once, none deduped", async () => {
    const w = world(["a", "b", "c", "d", "e"]);
    const done = await (await startWave({ repositoryId: "r", n: 5, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("done");
    expect(w.firsts).toHaveLength(5);
    expect(w.posts).toHaveLength(15);
    // Every gate range is distinct: the tip moves with each merge, so there is nothing to dedupe.
    expect(new Set(w.posts.map((p) => `${p.id}:${p.post.base}..${p.post.head}`)).size).toBe(15);
    expect(w.peak()).toBeLessThanOrEqual(2);
  });

  it("a failing re-verify of the member just merged names that member and merges nothing after it", async () => {
    const w = world(["a", "b", "c"], {});
    const verify = w.deps.verify;
    w.deps.verify = async (id, signal, post) => (id === "b" && post?.head === "t2" ? { state: "failed", stale: false, head: post.head } : verify(id, signal, post));
    const done = await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(w.merged).toEqual(["a", "b"]);
    expect(done.note).toContain('merging "Task b"');
    expect(done.note).not.toContain("found");
    expect(done.members.find((m) => m.taskId === "b")!.detail).toMatch(/was failed/);
  });

  it("a gate verification that rejects for an earlier member halts and names it, rather than reading as a pass", async () => {
    const w = world(["a", "b"]);
    const verify = w.deps.verify;
    w.deps.verify = async (id, signal, post) => { if (id === "a" && post?.head === "t2") throw new Error("sidecar down"); return verify(id, signal, post); };
    const done = await (await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("halted");
    expect(done.note).toMatch(/found "Task a" could not run \(sidecar down\)/);
  });
});

describe("the gate outcome is durable before the wave can go on", () => {
  // BREAK: `merged` is written (gateMerge) before the halt is. A process that dies between the two leaves a
  // running record in which the landed member is `merged` with no halt, and a resume merges the next member
  // over a regression the gate had found.
  it.fails("a crash between the failing member's `merged` write and the halt does not let a resume merge the next member", async () => {
    const w = world(["a", "b", "c"]);
    const verify = w.deps.verify;
    w.deps.verify = async (id, signal, post) => (id === "a" && post?.head === "t2" ? { state: "failed", stale: false, head: post.head } : verify(id, signal, post));
    let crashed = false;
    const first: WaveDeps = { ...w.deps, persist: async (record) => {
      await w.deps.persist(record);
      const bMerged = record.members.find((m) => m.taskId === "b")?.phase === "merged";
      if (bMerged && record.state === "running" && !crashed) { crashed = true; throw new Error("process died"); }
    } };
    await (await startWave({ repositoryId: "r", n: 3, waves: 1 }, first)).finished;
    const saved = w.persisted.findLast((r) => r.state === "running")!;
    expect(saved.members.find((m) => m.taskId === "b")!.phase).toBe("merged");
    const again = world(["a", "b", "c"]);
    await (await resumeWave(structuredClone(saved), again.deps)).finished;
    expect(again.merged, "c must not merge over a regression the gate found in a").toEqual([]);
  });
});

describe("persist failures and same-tick settles", () => {
  it("two members finishing in the same tick are both recorded, in strictly increasing seq order", async () => {
    const w = world(["a", "b"]);
    const done = await (await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps)).finished;
    expect(done.state).toBe("done");
    const seqs = w.persisted.map((r) => r.seq);
    expect(seqs).toEqual([...seqs].sort((x, y) => x - y));
    expect(new Set(seqs).size).toBe(seqs.length);
    expect(w.persisted.every((r) => r.members.length === 2 || r.members.length === 0)).toBe(true);
    expect(phases(done)).toEqual({ a: "merged", b: "merged" });
  });

  it("a persist that rejects while verifications settle fails the wave honestly: no merge after it, no member dropped", async () => {
    const w = world(["a", "b"]);
    let calls = 0;
    const persist = w.deps.persist;
    w.deps.persist = async (record) => {
      calls += 1;
      if (record.members.some((m) => m.phase === "ready")) throw new Error("storage busy");
      return persist(record);
    };
    const runner = await startWave({ repositoryId: "r", n: 2, waves: 1 }, w.deps);
    const done = await runner.finished;
    expect(calls).toBeGreaterThan(0);
    expect(done.state).toBe("failed");
    expect(done.note).toMatch(/could not be kept|could not be saved/);
    expect(done.members.map((m) => m.taskId)).toEqual(["a", "b"]);
    expect(w.merged).toEqual([]);
  });
});

describe("stop leaves Running within seconds", () => {
  const stopWithin = async (hang: (deps: WaveDeps) => void, ready: (runner: Awaited<ReturnType<typeof startWave>>) => boolean) => {
    vi.useFakeTimers();
    try {
      const w = world(["a", "b", "c"]);
      w.deps.sleep = (ms, signal) => new Promise<void>((resolve) => { const t = setTimeout(resolve, ms); signal.addEventListener("abort", () => { clearTimeout(t); resolve(); }, { once: true }); });
      hang(w.deps);
      const runner = await startWave({ repositoryId: "r", n: 3, waves: 1 }, w.deps);
      for (let i = 0; i < 50 && !ready(runner); i += 1) await vi.advanceTimersByTimeAsync(10);
      expect(ready(runner)).toBe(true);
      const first = runner.cancel(), second = runner.cancel();
      await vi.advanceTimersByTimeAsync(2_000);
      await Promise.all([first, second]);
      expect(runner.record().state).toBe("cancelled");
      expect(runner.stopping()).toBe(false);
    } finally { vi.useRealTimers(); }
  };

  it("during prepare, twice in a row", async () => {
    await stopWithin((d) => { d.launch = (_id, _ids, signal) => untilAborted(signal); d.getRun = async () => null; }, () => true);
  });

  it("during concurrent verifies, twice in a row", async () => {
    await stopWithin((d) => { d.verify = (_id, signal) => untilAborted(signal); }, (r) => r.record().members.filter((m) => m.phase === "verifying").length === 3);
  });

  it("during the gate with several members in it, twice in a row", async () => {
    let inGate = 0;
    await stopWithin((d) => {
      const verify = d.verify;
      d.verify = (id, signal, post) => { if (!post) return verify(id, signal, post); inGate += 1; return untilAborted(signal); };
    }, () => inGate > 0);
  });
});

describe("the host's verification limit", () => {
  // Only a literal `2` is pinned in waveHost.test.ts. The Rust side is the sweep's bound, and `run_now`
  // (what a wave calls) does not consult it, so the copy is a convention, not an enforcement.
  it("equals verification_driver.rs MAX_ACTIVE", () => {
    const source = readFileSync(new URL("../../../src-tauri/src/workbench/verification_driver.rs", import.meta.url), "utf8");
    const found = /const MAX_ACTIVE: usize = (\d+);/.exec(source);
    expect(found, "MAX_ACTIVE not found").toBeTruthy();
    expect(HOST_VERIFY_LIMIT).toBe(Number(found![1]));
  });
});

describe("record decoding across versions", () => {
  const member = { taskId: "a", title: "T", phase: "running", runId: "r", requestId: "q", cwd: "/wt", head: null, detail: "", startedAt: 1 };
  const base = { v: 1, id: "w", repositoryId: "r", n: 1, batchesLeft: 0, batch: 1, state: "running", members: [member], attempted: ["a"], note: "", seq: 3 };
  const line = (r: unknown) => `${WAVE_MARK}${JSON.stringify(r)}`;

  it("decodes a record main's encoder wrote, every phase it knew", () => {
    for (const phase of ["launching", "running", "verifying", "ready", "merging", "merged", "held", "stopped"]) {
      expect(decodeWave(line({ ...base, members: [{ ...member, phase }] }))?.members[0]?.phase, phase).toBe(phase);
    }
  });

  it("does not throw on an unknown phase, an unknown state, or extra fields", () => {
    expect(() => decodeWave(line({ ...base, members: [{ ...member, phase: "from_the_future" }] }))).not.toThrow();
    expect(() => decodeWave(line({ ...base, state: "paused" }))).not.toThrow();
    expect(decodeWave(line({ ...base, extra: 1, members: [{ ...member, extra: 2 }] }))).not.toBeNull();
  });

  // BREAK: an unknown member phase decodes to null, which `findActiveWave` skips as "not a running wave".
  // A wave a newer build left running is then invisible and `complete` stays true, so `beginWave` starts a
  // second wave beside it.
  it.fails("a running wave with a phase this build does not know is not reported as no wave with a complete look", async () => {
    const stored = line({ ...base, members: [{ ...member, phase: "from_the_future" }] });
    const api: WaveTaskApi = {
      getTask: async () => ({ id: "a", revision: 1, labels: [waveLabel("w")], logs: stored }) as never,
      saveTask: async () => { throw new Error("unused"); },
      listCards: async (status) => (status === "in_progress" ? { items: [{ id: "a", labels: [waveLabel("w")] }] as never, has_more: false, next_cursor: null } : { items: [], has_more: false, next_cursor: null }),
    };
    expect(WAVE_LABEL_PREFIX.length).toBeGreaterThan(0);
    const found = await findActiveWave(api);
    expect(found.record !== null || found.complete === false).toBe(true);
  });

  it("round-trips through the encoder", () => {
    const r: WaveRecord = { ...(base as unknown as WaveRecord), members: [{ ...(member as unknown as WaveRecord["members"][number]), phase: "verifying_merge", mergeBefore: "a".repeat(40), mergeAfter: "b".repeat(40) }] };
    expect(decodeWave(encodeWave(r))).toEqual(r);
  });
});

describe("S5-12 scope identity", () => {
  const ci = { caseInsensitive: true };
  it("folds the cases a filesystem folds, and only those", () => {
    expect(scopesOverlap(["src/A.ts"], ["src/a.ts"], ci)).toBe(true);
    expect(scopesOverlap(["src/lib/"], ["./src/lib/x.ts"], ci)).toBe(true);
    expect(scopesOverlap(["./src/a.ts"], ["src/a.ts"], ci)).toBe(true);
    expect(scopesOverlap(["src//a.ts"], ["src/a.ts"], ci)).toBe(true);
    expect(scopesOverlap(["src/café.ts"], ["src/café.ts"], ci)).toBe(true);
    // Not equal on APFS or NTFS: sharp s is not "ss", and Turkish dotless i is not "i".
    expect(scopesOverlap(["src/straße.ts"], ["src/STRASSE.ts"], ci)).toBe(false);
    expect(scopesOverlap(["src/ı.ts"], ["src/I.ts"], ci)).toBe(false);
    expect(planBatch([task("a"), task("b")], new Map([["a", ["Src/A.ts/"]], ["b", ["./src/a.ts"]]]), 2, new Set(), ci).chosen.map((c) => c.id)).toEqual(["a"]);
  });

  // BREAK: only a leading `./` is dropped, so an inner `.` segment (src/./a.ts) or a `..` segment keeps two
  // spellings of one file apart and two tasks on the same file launch together.
  it.fails("src/./a.ts and src/x/../a.ts are the same file as src/a.ts", () => {
    expect(scopesOverlap(["src/./a.ts"], ["src/a.ts"], ci)).toBe(true);
    expect(scopesOverlap(["src/x/../a.ts"], ["src/a.ts"], ci)).toBe(true);
  });
});
