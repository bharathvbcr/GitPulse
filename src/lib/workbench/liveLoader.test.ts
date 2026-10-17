import { afterEach, describe, expect, it, vi } from "vitest";
import { createLiveLoader } from "./liveLoader";

afterEach(() => vi.useRealTimers());

function setup(overrides: { canRun?: () => boolean } = {}) {
  const gates: Array<{ resolve: () => void; reject: (e: unknown) => void; isCurrent: () => boolean }> = [];
  const applied: string[] = [];
  const errors: unknown[] = [];
  const loader = createLiveLoader<string>({
    canRun: overrides.canRun ?? (() => true),
    read: (mode, isCurrent) =>
      new Promise<void>((resolve, reject) => {
        gates.push({ resolve: () => { if (isCurrent()) applied.push(mode ?? "default"); resolve(); }, reject, isCurrent });
      }),
    onError: (cause) => errors.push(cause),
  });
  return { loader, gates, applied, errors };
}

describe("createLiveLoader", () => {
  it("runs one read at a time and re-runs once, with no mode, for loads that arrived meanwhile", async () => {
    const { loader, gates, applied } = setup();
    const first = loader.load("first");
    void loader.load("more");
    void loader.load("more");
    expect(gates).toHaveLength(1);
    gates[0].resolve();
    await first;
    await Promise.resolve();
    expect(gates).toHaveLength(2);
    gates[1].resolve();
    await new Promise((r) => setTimeout(r, 0));
    expect(applied).toEqual(["first", "default"]);
  });

  it("a reset orphans the read in flight, and its failure is not reported", async () => {
    const { loader, gates, applied, errors } = setup();
    const stale = loader.load("first");
    loader.reset();
    expect(gates[0].isCurrent()).toBe(false);
    gates[0].reject(new Error("late"));
    await stale;
    expect(errors).toEqual([]);
    const fresh = loader.load("first");
    gates[1].resolve();
    await fresh;
    expect(applied).toEqual(["first"]);
  });

  it("reports a current failure and keeps the loop usable", async () => {
    const { loader, gates, errors } = setup();
    const run = loader.load();
    gates[0].reject(new Error("boom"));
    await run;
    expect(errors).toHaveLength(1);
    void loader.load();
    expect(gates).toHaveLength(2);
  });

  it("does nothing while it cannot run, and nothing after dispose", async () => {
    vi.useFakeTimers();
    let run = false;
    const { loader, gates } = setup({ canRun: () => run });
    await loader.load();
    loader.schedule();
    vi.advanceTimersByTime(1000);
    expect(gates).toHaveLength(0);
    run = true;
    loader.schedule();
    loader.schedule();
    vi.advanceTimersByTime(199);
    expect(gates).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(gates).toHaveLength(1);
    loader.dispose();
    expect(loader.isDisposed()).toBe(true);
    await loader.load();
    loader.schedule();
    vi.advanceTimersByTime(1000);
    expect(gates).toHaveLength(1);
  });

  // A store written about once a second (a background sweep, an agent) sent a
  // change event each time, and every one became a full re-read.
  it("spaces change-driven reads by minGapMs however often changes arrive", async () => {
    vi.useFakeTimers();
    const reads: number[] = [];
    const loader = createLiveLoader({
      canRun: () => true,
      read: async () => { reads.push(Date.now()); },
      onError: () => {},
      minGapMs: 2_000,
    });
    const start = Date.now();
    for (let t = 0; t < 10_000; t += 1_000) {
      loader.schedule();
      await vi.advanceTimersByTimeAsync(1_000);
    }
    expect(reads.length).toBeGreaterThanOrEqual(4);
    expect(reads.length).toBeLessThanOrEqual(6);
    for (let i = 1; i < reads.length; i++) expect(reads[i] - reads[i - 1]).toBeGreaterThanOrEqual(2_000);
    expect(reads[0] - start).toBeLessThanOrEqual(200);
  });

  // The debounce restarted its timer on every change, so changes arriving
  // faster than its delay put the read off for as long as they kept coming.
  it("a steady stream of changes does not starve the read", async () => {
    vi.useFakeTimers();
    const { loader, gates } = setup();
    for (let t = 0; t < 1_000; t += 50) {
      loader.schedule();
      await vi.advanceTimersByTimeAsync(50);
    }
    expect(gates.length).toBeGreaterThanOrEqual(1);
  });

  // When the store is slow a read outlasts the gap. A change landing mid-read
  // used to queue an immediate re-run, so reads ran back to back for as long
  // as the store stayed slow — the moment it most needed the rest.
  it("a read slower than the gap is followed by the gap, not by the next read", async () => {
    vi.useFakeTimers();
    const spans: Array<[number, number]> = [];
    const loader = createLiveLoader({
      canRun: () => true,
      read: async () => {
        const start = Date.now();
        await new Promise((resolve) => setTimeout(resolve, 3_000));
        spans.push([start, Date.now()]);
      },
      onError: () => {},
      minGapMs: 1_500,
    });
    for (let t = 0; t < 20_000; t += 250) {
      loader.schedule();
      await vi.advanceTimersByTimeAsync(250);
    }
    expect(spans.length).toBeGreaterThanOrEqual(3);
    for (let i = 1; i < spans.length; i++) expect(spans[i][0] - spans[i - 1][1]).toBeGreaterThanOrEqual(1_500);
  });

  it("an explicit load is never held back by the gap", async () => {
    vi.useFakeTimers();
    const { loader, gates } = setup();
    loader.schedule();
    await vi.advanceTimersByTimeAsync(200);
    gates[0].resolve();
    await vi.advanceTimersByTimeAsync(0);
    void loader.load("now");
    expect(gates).toHaveLength(2);
  });

  it("a reset cancels a pending coalesced refresh", () => {
    vi.useFakeTimers();
    const { loader, gates } = setup();
    loader.schedule();
    loader.reset();
    vi.advanceTimersByTime(1000);
    expect(gates).toHaveLength(0);
  });
});
