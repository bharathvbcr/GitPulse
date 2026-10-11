import { describe, expect, it } from "vitest";
import type { Task, TaskCard, TaskDraft, TaskStatus } from "./client";
import { DISCOVERY_PAGES, WAVE_MARK, createWavePersister, decodeWave, encodeWave, findActiveWave, waveLabel, type WaveTaskApi } from "./waveStore";
import type { WaveMember, WaveRecord } from "./waveLaunch";

const member = (taskId: string, phase: WaveMember["phase"] = "running"): WaveMember => ({ taskId, title: `T ${taskId}`, phase, runId: `run-${taskId}`, requestId: `req-${taskId}`, cwd: `/wt/${taskId}`, head: null, detail: "", startedAt: 5 });
const record = (patch: Partial<WaveRecord> = {}): WaveRecord => ({ v: 1, id: "w1", repositoryId: "r", n: 3, batchesLeft: 1, batch: 1, state: "running", members: [member("a"), member("b")], attempted: ["a", "b"], note: "", seq: 4, ...patch });

/** An in-memory board: cards by id, each with a revision; a conflict hook can move a card under a write. */
function board(statusOf: Record<string, TaskStatus> = {}) {
  const tasks = new Map<string, Task>();
  const saves: string[] = [];
  const api: WaveTaskApi & { conflictOnce?: string } = {
    getTask: async (id) => { const t = tasks.get(id); if (!t) throw new Error(`no ${id}`); return structuredClone(t); },
    saveTask: async (id, revision, draft: TaskDraft) => {
      const t = tasks.get(id)!;
      if (api.conflictOnce === id) { api.conflictOnce = undefined; t.revision += 1; throw new Error("revision_conflict"); }
      if (t.revision !== revision) throw new Error("revision_conflict");
      const next = { ...t, ...draft, id, revision: revision + 1 } as Task;
      tasks.set(id, next); saves.push(id);
      return next;
    },
    listCards: async (status, cursor) => {
      const all = [...tasks.values()].filter((t) => (statusOf[t.id] ?? "in_progress") === status).map((t) => t as TaskCard);
      const start = cursor ? Number(cursor) : 0;
      const items = all.slice(start, start + 2);
      return { items, has_more: start + 2 < all.length, next_cursor: start + 2 < all.length ? String(start + 2) : null };
    },
  };
  const add = (id: string, extra: Partial<Task> = {}) => tasks.set(id, { id, revision: 1, title: id, labels: [], logs: "", status: statusOf[id] ?? "in_progress", ...extra } as unknown as Task);
  return { api, tasks, add, saves };
}

describe("record codec", () => {
  it("round-trips a record through a card's logs, beside other log text", () => {
    const r = record();
    expect(decodeWave(`pasted trace\n${encodeWave(r)}\nmore`)).toEqual(r);
  });

  it("refuses anything it cannot trust, rather than guessing", () => {
    const good = JSON.parse(JSON.stringify(record()));
    const bad: unknown[] = [
      { ...good, v: 2 }, { ...good, state: "paused" }, { ...good, n: 0 }, { ...good, n: 6 }, { ...good, seq: -1 }, { ...good, seq: 1.5 },
      { ...good, members: Array.from({ length: 6 }, (_, i) => member(`m${i}`)) }, { ...good, members: [{ ...member("a"), phase: "teleporting" }] },
      { ...good, members: [{ ...member("a"), runId: "" }] }, { ...good, members: [{ ...member("a"), cwd: 7 }] }, { ...good, attempted: [1] },
      { ...good, id: "" }, null, [], "x",
    ];
    for (const value of bad) expect(decodeWave(`${WAVE_MARK}${JSON.stringify(value)}`), JSON.stringify(value).slice(0, 60)).toBeNull();
    expect(decodeWave(`${WAVE_MARK}{not json`)).toBeNull();
    expect(decodeWave(undefined)).toBeNull();
    expect(decodeWave("no line here")).toBeNull();
  });
});

describe("persister", () => {
  it("labels the current batch and keeps one record line, replacing the last", async () => {
    const b = board(); b.add("a", { logs: "an agent's paste" }); b.add("b");
    const persist = createWavePersister(b.api);
    await persist(record({ seq: 1 }));
    await persist(record({ seq: 2 }));
    const a = b.tasks.get("a")!;
    expect(a.labels).toEqual([waveLabel("w1")]);
    expect(a.logs!.split("\n").filter((l) => l.startsWith(WAVE_MARK))).toHaveLength(1);
    expect(a.logs).toContain("an agent's paste");
    expect(decodeWave(a.logs)!.seq).toBe(2);
  });

  it("moves the label to the next batch, and strips every label when the wave stops running", async () => {
    const b = board(); for (const id of ["a", "b", "c"]) b.add(id);
    const persist = createWavePersister(b.api);
    await persist(record({ seq: 1 }));
    await persist(record({ seq: 2, batch: 2, members: [member("c")] }));
    expect(b.tasks.get("a")!.labels).toEqual([]);
    expect(b.tasks.get("c")!.labels).toEqual([waveLabel("w1")]);
    await persist(record({ seq: 3, batch: 2, state: "done", members: [member("c", "merged")] }));
    expect(b.tasks.get("c")!.labels).toEqual([]);
    expect(decodeWave(b.tasks.get("c")!.logs)!.state).toBe("done");
  });

  it("re-reads a card that moved under the write once, and fails loudly on a second conflict", async () => {
    const b = board(); b.add("a"); b.add("b");
    const persist = createWavePersister(b.api);
    b.api.conflictOnce = "a";
    await persist(record());
    expect(b.tasks.get("a")!.labels).toEqual([waveLabel("w1")]);
    const failing = board(); failing.add("a"); failing.add("b");
    failing.api.saveTask = async () => { throw new Error("revision_conflict"); };
    await expect(createWavePersister(failing.api)(record())).rejects.toThrow("revision_conflict");
  });
});

describe("finding the wave a restart should continue", () => {
  it("returns the highest-seq running record among labelled cards", async () => {
    const b = board(); b.add("a"); b.add("b"); b.add("z");
    await createWavePersister(b.api)(record({ seq: 3 }));
    // b's card is behind: it was written at seq 1 and then lost its update.
    const old = b.tasks.get("b")!; old.logs = encodeWave(record({ seq: 1 }));
    const found = await findActiveWave(b.api);
    expect(found.record!.seq).toBe(3);
    expect(found!.complete).toBe(true);
  });

  it("ignores finished waves, undecodable lines and a label that disagrees with its record", async () => {
    const b = board();
    b.add("done", { labels: [waveLabel("w1")], logs: encodeWave(record({ state: "done" })) });
    b.add("junk", { labels: [waveLabel("w2")], logs: `${WAVE_MARK}{oops` });
    b.add("liar", { labels: [waveLabel("w9")], logs: encodeWave(record({ id: "w1" })) });
    b.add("plain");
    expect(await findActiveWave(b.api)).toEqual({ record: null, complete: true });
  });

  it("says it looked at only part of the board when a status runs past its page cap", async () => {
    const b = board();
    for (let i = 0; i < (DISCOVERY_PAGES + 1) * 2; i += 1) b.add(`t${i}`);
    b.add("hit", { labels: [waveLabel("w1")], logs: encodeWave(record()) });
    // The labelled card is past the cap: "none found" is reported as a partial look, never as none.
    expect(await findActiveWave(b.api)).toEqual({ record: null, complete: false });
  });
});
