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

  it("a reset cancels a pending coalesced refresh", () => {
    vi.useFakeTimers();
    const { loader, gates } = setup();
    loader.schedule();
    loader.reset();
    vi.advanceTimersByTime(1000);
    expect(gates).toHaveLength(0);
  });
});
