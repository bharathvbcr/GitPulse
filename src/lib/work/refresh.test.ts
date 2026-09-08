import { afterEach, describe, expect, it, vi } from "vitest";
import { createWorkRefresh, type WorkRefreshObserver } from "./refresh";
import { projectWork, type WorkInputs } from "./projection";
import type { CollisionRisk } from "../insights/types";
import { WORK_TIMEOUT_MS } from "./request";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
const ok = { ok: true, present: true, detail: "" };
const input: WorkInputs = { leases: [], titles: {}, worktrees: [], bindings: {}, pullRequests: [], runs: [], events: [], grants: [], operations: {}, sources: { tasks: ok, worktrees: ok, github: ok, ledger: ok, grants: ok } };
const projection = projectWork(input);
const risk: CollisionRisk = { ok: true, error: "", overlapping_files: 0, worktrees_involved: 0, scanned_worktrees: 1, unscanned_worktrees: 0, failed_worktrees: 0, truncated: false, items: [] };
function observer(): WorkRefreshObserver { return { projection: vi.fn(), collisions: vi.fn(), finished: vi.fn() }; }
const tick = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };
afterEach(() => vi.useRealTimers());

describe("Overview refresh lifecycle", () => {
  it("publishes local rows before the overlap scan, and finishes only after both", async () => {
    const collision = deferred<CollisionRisk>();
    const view = observer();
    const worker = createWorkRefresh({ load: async () => projection, collisions: () => collision.promise });
    worker.request("/a", view);
    await tick();
    expect(view.projection).toHaveBeenCalledOnce();
    expect(view.finished).not.toHaveBeenCalled();
    collision.resolve(risk);
    await tick();
    expect(view.collisions).toHaveBeenCalledWith(risk, null);
    expect(view.finished).toHaveBeenCalledWith(null);
  });

  it("coalesces 10000 updates into one queued refresh without starving collisions", async () => {
    const first = deferred<typeof projection>();
    const load = vi.fn().mockImplementationOnce(() => first.promise).mockResolvedValue(projection);
    const collisions = vi.fn().mockResolvedValue(risk);
    const worker = createWorkRefresh({ load, collisions });
    worker.request("/a", observer());
    await tick();
    let last = observer();
    for (let i = 0; i < 10_000; i++) { last = observer(); worker.request("/a", last); }
    expect(load).toHaveBeenCalledTimes(1);
    first.resolve(projection);
    await tick(); await tick();
    expect(load).toHaveBeenCalledTimes(2);
    expect(collisions).toHaveBeenCalledTimes(2);
    expect(last.finished).toHaveBeenCalledWith(null);
  });

  it("suppresses stale results on switch and retains only the latest requested repository", async () => {
    const first = deferred<typeof projection>();
    const load = vi.fn().mockImplementationOnce(() => first.promise).mockResolvedValue(projection);
    const collisions = vi.fn().mockResolvedValue(risk);
    const worker = createWorkRefresh({ load, collisions });
    const old = observer(); const skipped = observer(); const next = observer();
    worker.request("/a", old); await tick();
    worker.request("/b", skipped); worker.request("/c", next);
    first.resolve(projection); await tick(); await tick();
    expect(load.mock.calls.map(call => call[0])).toEqual(["/a", "/c"]);
    expect(old.projection).not.toHaveBeenCalled();
    expect(skipped.projection).not.toHaveBeenCalled();
    expect(collisions).toHaveBeenCalledTimes(1);
    expect(next.finished).toHaveBeenCalledWith(null);
  });

  it("cancels publication and secondary scans on close or unmount", async () => {
    const first = deferred<typeof projection>();
    const view = observer(); const collisions = vi.fn().mockResolvedValue(risk);
    const worker = createWorkRefresh({ load: () => first.promise, collisions });
    worker.request("/a", view); await tick(); worker.cancel(); first.resolve(projection); await tick();
    expect(view.projection).not.toHaveBeenCalled();
    expect(view.finished).not.toHaveBeenCalled();
    expect(collisions).not.toHaveBeenCalled();
  });

  it("settles a hung collision scan and ignores its late answer", async () => {
    vi.useFakeTimers();
    const collision = deferred<CollisionRisk>(); const view = observer();
    createWorkRefresh({ load: async () => projection, collisions: () => collision.promise }).request("/a", view);
    await tick(); await vi.advanceTimersByTimeAsync(WORK_TIMEOUT_MS);
    expect(view.collisions).toHaveBeenCalledWith(null, expect.stringContaining("deadline"));
    expect(view.finished).toHaveBeenCalledOnce();
    collision.resolve(risk); await tick();
    expect(view.collisions).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("reports malformed collision results without calling them no overlap", async () => {
    const view = observer();
    createWorkRefresh({ load: async () => projection, collisions: async () => ({ ...risk, scanned_worktrees: NaN }) }).request("/a", view);
    await tick();
    expect(view.collisions).toHaveBeenCalledWith(null, expect.stringContaining("invalid response"));
    expect(view.finished).toHaveBeenCalledWith(null);
  });
});
