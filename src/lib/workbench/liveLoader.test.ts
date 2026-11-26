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

/**
 * GitPulse on screen but not focused — an agent filing tasks from another
 * app's terminal while the board sits beside it — is the commonest time a
 * change arrives. The loader used to treat lost focus as "in the background",
 * so the change was dropped and the board only caught up on the next click
 * into the window.
 */
describe("createLiveLoader on an unfocused but visible window", () => {
  function fakeWindow() {
    const handlers = new Map<string, Set<() => void>>();
    const target = {
      addEventListener: (type: string, fn: () => void) => { (handlers.get(type) ?? handlers.set(type, new Set()).get(type)!).add(fn); },
      removeEventListener: (type: string, fn: () => void) => { handlers.get(type)?.delete(fn); },
      fire: (type: string) => { for (const fn of [...(handlers.get(type) ?? [])]) fn(); },
    };
    return target;
  }

  afterEach(async () => {
    vi.unstubAllGlobals();
    (await import("../runtime/foreground")).resetForegroundFocus();
  });

  it("reports the window as visible after a blur and keeps reading changes", async () => {
    vi.useFakeTimers();
    const doc = { hidden: false, visibilityState: "visible", hasFocus: () => false, ...fakeWindow() };
    const frame = fakeWindow();
    vi.stubGlobal("document", doc);
    vi.stubGlobal("window", frame);
    const { liveOnScreen } = await import("./liveLoader");
    let visible: boolean | null = null;
    const { loader, gates } = setup({ canRun: () => visible !== false && liveOnScreen() });
    const disconnect = loader.connect({ onVisibility: (next) => { visible = next; }, onUnavailable: () => {} });
    frame.fire("blur");
    expect(visible, "a blurred window that is still on screen is visible").toBe(true);
    loader.schedule();
    vi.advanceTimersByTime(1_000);
    expect(gates, "the change is read while the window is unfocused").toHaveLength(1);
    disconnect();
  });

  it("re-reads when the window takes focus or is shown again, and not on a blur that leaves it on screen", async () => {
    vi.useFakeTimers();
    const doc = { hidden: false, visibilityState: "visible", hasFocus: () => true, ...fakeWindow() };
    const frame = fakeWindow();
    vi.stubGlobal("document", doc);
    vi.stubGlobal("window", frame);
    const { liveOnScreen } = await import("./liveLoader");
    const { loader, gates } = setup({ canRun: () => liveOnScreen() });
    const disconnect = loader.connect({ onVisibility: () => {}, onUnavailable: () => {} });
    frame.fire("blur");
    vi.advanceTimersByTime(2_000);
    expect(gates, "a blur on screen costs no read: changes still arrive as events").toHaveLength(0);
    frame.fire("focus");
    vi.advanceTimersByTime(2_000);
    expect(gates, "taking focus back re-reads once").toHaveLength(1);
    gates[0].resolve();
    disconnect();
  });

  it("pauses only once the window is hidden, and reads again when it is shown", async () => {
    vi.useFakeTimers();
    const doc = { hidden: true, visibilityState: "hidden", hasFocus: () => false, ...fakeWindow() };
    vi.stubGlobal("document", doc);
    vi.stubGlobal("window", fakeWindow());
    const { liveOnScreen } = await import("./liveLoader");
    let visible: boolean | null = null;
    const { loader, gates } = setup({ canRun: () => liveOnScreen() });
    const disconnect = loader.connect({ onVisibility: (next) => { visible = next; }, onUnavailable: () => {} });
    loader.schedule();
    vi.advanceTimersByTime(1_000);
    expect(gates).toHaveLength(0);
    doc.hidden = false; doc.visibilityState = "visible";
    doc.fire("visibilitychange");
    expect(visible).toBe(true);
    vi.advanceTimersByTime(1_000);
    expect(gates, "showing the window reads what changed while it was hidden").toHaveLength(1);
    disconnect();
  });
});

describe("createLiveLoader retry", () => {
  /** A source whose answers the test chooses: each read takes the next outcome, and its start time is logged. */
  function failing(outcomes: Array<"ok" | "failed" | "throw">, retry?: { baseMs: number; maxMs: number }) {
    const starts: number[] = [];
    let call = 0;
    const loader = createLiveLoader({
      canRun: () => true,
      read: async () => {
        starts.push(Date.now());
        const outcome = outcomes[Math.min(call++, outcomes.length - 1)];
        if (outcome === "throw") throw new Error("boom");
        return outcome === "failed" ? "failed" : undefined;
      },
      onError: () => {},
      minGapMs: 0,
      retry,
    });
    return { loader, starts };
  }
  const gapsOf = (starts: number[]) => starts.slice(1).map((at, i) => at - starts[i]);

  it("retries a failed read at a doubling interval up to the cap", async () => {
    vi.useFakeTimers();
    const { loader, starts } = failing(["failed"], { baseMs: 1_000, maxMs: 8_000 });
    void loader.load();
    await vi.advanceTimersByTimeAsync(40_000);
    expect(gapsOf(starts).slice(0, 5)).toEqual([1_000, 2_000, 4_000, 8_000, 8_000]);
    loader.dispose();
  });

  it("counts a thrown read as a failure too", async () => {
    vi.useFakeTimers();
    const { loader, starts } = failing(["throw"], { baseMs: 1_000, maxMs: 8_000 });
    void loader.load();
    await vi.advanceTimersByTimeAsync(8_000);
    expect(gapsOf(starts)).toEqual([1_000, 2_000, 4_000]);
    loader.dispose();
  });

  it("starts the schedule over once a read lands", async () => {
    vi.useFakeTimers();
    const { loader, starts } = failing(["failed", "failed", "failed", "ok", "failed"], { baseMs: 1_000, maxMs: 8_000 });
    void loader.load();
    await vi.advanceTimersByTimeAsync(7_000);
    expect(gapsOf(starts)).toEqual([1_000, 2_000, 4_000]);
    // The fourth read landed: nothing is owed, and the next change begins again at the base interval.
    await vi.advanceTimersByTimeAsync(20_000);
    expect(starts).toHaveLength(4);
    loader.schedule();
    await vi.advanceTimersByTimeAsync(200);
    expect(starts).toHaveLength(5);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(starts).toHaveLength(6);
    loader.dispose();
  });

  it("holds changes that arrive during the backoff to it, and never holds back load()", async () => {
    vi.useFakeTimers();
    const { loader, starts } = failing(["failed"], { baseMs: 1_000, maxMs: 8_000 });
    void loader.load();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(starts).toHaveLength(2);
    // Failed twice: the next try is due in 2 s. Changes in that window do not bring it forward.
    for (let t = 0; t < 1_500; t += 100) { loader.schedule(); await vi.advanceTimersByTimeAsync(100); }
    expect(starts).toHaveLength(2);
    await loader.load();
    expect(starts).toHaveLength(3);
    loader.dispose();
  });

  it("does not retry at all unless asked to", async () => {
    vi.useFakeTimers();
    const { loader, starts } = failing(["failed"]);
    void loader.load();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(starts).toHaveLength(1);
    loader.dispose();
  });

  it("a reset forgets the failures", async () => {
    vi.useFakeTimers();
    const { loader, starts } = failing(["failed"], { baseMs: 1_000, maxMs: 8_000 });
    void loader.load();
    await vi.advanceTimersByTimeAsync(3_000);
    expect(starts).toHaveLength(3);
    loader.reset();
    const before = starts.length;
    void loader.load();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(starts.length - before).toBe(2);
    loader.dispose();
  });
});
