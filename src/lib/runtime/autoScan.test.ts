import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  AUTO_SCAN_SETTLE_MS,
  REMOUNT_GRACE_MS,
  SCAN_FRESH_MS,
  createScanJoin,
  isScanStale,
  scheduleAutoScan,
  setAutoScanSettleMs,
} from "./autoScan";

/** Lets the settle's `whenDocumentShown().then(...)` microtasks run. */
const flush = async () => {
  for (let i = 0; i < 4; i++) await Promise.resolve();
};

/** A key no other test has used: waits are module state, shared across tests in a file. */
let keySeq = 0;
const freshKey = () => `test:${++keySeq}`;

describe("isScanStale", () => {
  it("treats an unstamped result as stale and a result at the limit as stale", () => {
    expect(isScanStale(0, 10_000)).toBe(true);
    expect(isScanStale(-1, 10_000)).toBe(true);
    expect(isScanStale(1_000, 1_000 + SCAN_FRESH_MS - 1)).toBe(false);
    expect(isScanStale(1_000, 1_000 + SCAN_FRESH_MS)).toBe(true);
  });
});

describe("createScanJoin", () => {
  it("joins a scan already running for the key instead of starting another", async () => {
    const join = createScanJoin<number>();
    let release: (value: number) => void = () => {};
    const start = vi.fn(() => new Promise<number>((resolve) => (release = resolve)));
    const first = join.run("a", start);
    const second = join.run("a", start);
    expect(join.has("a")).toBe(true);
    expect(start).toHaveBeenCalledTimes(1);
    release(7);
    expect(await Promise.all([first, second])).toEqual([7, 7]);
    expect(join.has("a")).toBe(false);
  });

  it("keys are independent, and a finished scan is not joined by a later one", async () => {
    const join = createScanJoin<string>();
    const start = vi.fn(async () => "done");
    await Promise.all([join.run("a", start), join.run("b", start)]);
    expect(start).toHaveBeenCalledTimes(2);
    await join.run("a", start);
    expect(start).toHaveBeenCalledTimes(3);
  });

  it("a failure reaches every joiner and is forgotten, so the next call retries", async () => {
    const join = createScanJoin<string>();
    const start = vi.fn(async () => {
      throw new Error("boom");
    });
    const results = await Promise.allSettled([join.run("a", start), join.run("a", start)]);
    expect(results.map((r) => r.status)).toEqual(["rejected", "rejected"]);
    expect(start).toHaveBeenCalledTimes(1);
    expect(join.has("a")).toBe(false);
    await expect(join.run("a", start)).rejects.toThrow("boom");
    expect(start).toHaveBeenCalledTimes(2);
  });
});

describe("scheduleAutoScan", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    setAutoScanSettleMs(null);
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    setAutoScanSettleMs(null);
  });

  it("starts nothing before the pane has settled, and one scan after", async () => {
    const run = vi.fn();
    scheduleAutoScan({ key: freshKey(), stillNeeded: () => true, run });
    await vi.advanceTimersByTimeAsync(AUTO_SCAN_SETTLE_MS - 1);
    expect(run).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    await flush();
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("a visit shorter than the settle starts no scan, however many times it repeats", async () => {
    const key = freshKey();
    const run = vi.fn();
    for (let visit = 0; visit < 5; visit++) {
      const cancel = scheduleAutoScan({ key, stillNeeded: () => true, run });
      await vi.advanceTimersByTimeAsync(400);
      cancel();
      await vi.advanceTimersByTimeAsync(REMOUNT_GRACE_MS + 50);
    }
    await vi.advanceTimersByTimeAsync(AUTO_SCAN_SETTLE_MS * 2);
    await flush();
    expect(run).not.toHaveBeenCalled();
  });

  it("rapid A to B to A switching leaves only the last pane's scan", async () => {
    const runA1 = vi.fn();
    const runB = vi.fn();
    const runA2 = vi.fn();
    const cancelA1 = scheduleAutoScan({ key: "pair:A", stillNeeded: () => true, run: runA1 });
    await vi.advanceTimersByTimeAsync(300);
    cancelA1();
    const cancelB = scheduleAutoScan({ key: "pair:B", stillNeeded: () => true, run: runB });
    await vi.advanceTimersByTimeAsync(300);
    cancelB();
    scheduleAutoScan({ key: "pair:A", stillNeeded: () => true, run: runA2 });
    await vi.advanceTimersByTimeAsync(AUTO_SCAN_SETTLE_MS);
    await flush();
    expect([runA1, runB, runA2].map((fn) => fn.mock.calls.length)).toEqual([0, 0, 1]);
  });

  it("a pane rebuilt mid-wait resumes the wait instead of restarting it", async () => {
    const key = freshKey();
    const first = vi.fn();
    const second = vi.fn();
    const cancelFirst = scheduleAutoScan({ key, stillNeeded: () => true, run: first });
    await vi.advanceTimersByTimeAsync(1_000);
    cancelFirst();
    scheduleAutoScan({ key, stillNeeded: () => true, run: second });
    await vi.advanceTimersByTimeAsync(AUTO_SCAN_SETTLE_MS - 1_000 - 1);
    expect(second).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    await flush();
    expect([first.mock.calls.length, second.mock.calls.length]).toEqual([0, 1]);
  });

  it("a return after the grace is a new visit and waits the whole settle", async () => {
    const key = freshKey();
    const run = vi.fn();
    const cancel = scheduleAutoScan({ key, stillNeeded: () => true, run });
    await vi.advanceTimersByTimeAsync(1_000);
    cancel();
    await vi.advanceTimersByTimeAsync(REMOUNT_GRACE_MS);
    scheduleAutoScan({ key, stillNeeded: () => true, run });
    await vi.advanceTimersByTimeAsync(AUTO_SCAN_SETTLE_MS - 1);
    expect(run).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    await flush();
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("two panes waiting on one key start once between them when the first answers the second", async () => {
    const key = freshKey();
    const run = vi.fn();
    let answered = false;
    const stillNeeded = () => !answered;
    scheduleAutoScan({ key, stillNeeded, run: () => { answered = true; run(); } });
    scheduleAutoScan({ key, stillNeeded, run: () => { answered = true; run(); } });
    await vi.advanceTimersByTimeAsync(AUTO_SCAN_SETTLE_MS);
    await flush();
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("does not start when the answer arrived while it waited", async () => {
    const run = vi.fn();
    let answered = false;
    scheduleAutoScan({ key: freshKey(), stillNeeded: () => !answered, run });
    answered = true;
    await vi.advanceTimersByTimeAsync(AUTO_SCAN_SETTLE_MS);
    await flush();
    expect(run).not.toHaveBeenCalled();
  });

  it("a cancel after the settle but before a hidden document is shown still stops it", async () => {
    const listeners: Array<() => void> = [];
    const doc = {
      hidden: true,
      visibilityState: "hidden",
      addEventListener: (_type: string, fn: () => void) => listeners.push(fn),
      removeEventListener: vi.fn(),
    };
    vi.stubGlobal("document", doc);
    const run = vi.fn();
    const cancel = scheduleAutoScan({ key: freshKey(), stillNeeded: () => true, run });
    await vi.advanceTimersByTimeAsync(AUTO_SCAN_SETTLE_MS);
    await flush();
    expect(run).not.toHaveBeenCalled();
    cancel();
    doc.hidden = false;
    doc.visibilityState = "visible";
    for (const fn of listeners) fn();
    await flush();
    expect(run).not.toHaveBeenCalled();
  });

  it("waits out a hidden document and starts once it is shown", async () => {
    const listeners: Array<() => void> = [];
    const doc = {
      hidden: true,
      visibilityState: "hidden",
      addEventListener: (_type: string, fn: () => void) => listeners.push(fn),
      removeEventListener: vi.fn(),
    };
    vi.stubGlobal("document", doc);
    const run = vi.fn();
    scheduleAutoScan({ key: freshKey(), stillNeeded: () => true, run });
    await vi.advanceTimersByTimeAsync(AUTO_SCAN_SETTLE_MS * 4);
    await flush();
    expect(run).not.toHaveBeenCalled();
    doc.hidden = false;
    doc.visibilityState = "visible";
    for (const fn of listeners) fn();
    await flush();
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("the settle can be shortened for a harness and restored", async () => {
    setAutoScanSettleMs(0);
    const run = vi.fn();
    scheduleAutoScan({ key: freshKey(), stillNeeded: () => true, run });
    await vi.advanceTimersByTimeAsync(0);
    await flush();
    expect(run).toHaveBeenCalledTimes(1);
  });
});
