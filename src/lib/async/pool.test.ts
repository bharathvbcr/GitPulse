import { describe, expect, it, vi } from "vitest";
import { DEFAULT_FAN_OUT, mapItems, mapWithConcurrency } from "./pool";

/** Resolves after `ms`, tracking peak overlap in the shared counter. */
function tracker() {
  let live = 0;
  let peak = 0;
  return {
    get peak() {
      return peak;
    },
    async run<T>(value: T): Promise<T> {
      live += 1;
      peak = Math.max(peak, live);
      await Promise.resolve();
      await Promise.resolve();
      live -= 1;
      return value;
    },
  };
}

describe("mapWithConcurrency", () => {
  it("returns results in index order regardless of completion order", async () => {
    const out = await mapWithConcurrency(5, 2, async (i) => {
      // Later indices settle first.
      await new Promise((r) => setTimeout(r, (5 - i) * 2));
      return i * 10;
    });
    expect(out).toEqual([0, 10, 20, 30, 40]);
  });

  /**
   * The regression this module exists for: an unbounded fan-out put hundreds
   * of git-spawning commands in flight at once and exhausted the process's
   * file descriptors.
   */
  it("never runs more than `concurrency` tasks at once", async () => {
    const t = tracker();
    await mapWithConcurrency(64, 4, (i) => t.run(i));
    expect(t.peak).toBeLessThanOrEqual(4);
    expect(t.peak).toBeGreaterThan(1);
  });

  it("visits every index exactly once", async () => {
    const seen: number[] = [];
    await mapWithConcurrency(20, 3, async (i) => {
      seen.push(i);
      return i;
    });
    expect([...seen].sort((a, b) => a - b)).toEqual(
      Array.from({ length: 20 }, (_, i) => i),
    );
    expect(new Set(seen).size).toBe(20);
  });

  it("rejects with the first failure and stops claiming new work", async () => {
    const task = vi.fn(async (i: number) => {
      if (i === 1) throw new Error("boom");
      return i;
    });
    await expect(mapWithConcurrency(50, 2, task)).rejects.toThrow("boom");
    // Workers stop claiming after the failure rather than draining all 50.
    expect(task.mock.calls.length).toBeLessThan(50);
  });

  it("treats an empty or non-finite count as no work", async () => {
    const task = vi.fn(async () => 1);
    expect(await mapWithConcurrency(0, 4, task)).toEqual([]);
    expect(await mapWithConcurrency(Number.NaN, 4, task)).toEqual([]);
    expect(task).not.toHaveBeenCalled();
  });

  /**
   * A NaN width must fall back, never propagate: `Array.from({ length: NaN })`
   * is empty, so the run would resolve with a hole-filled array while nothing
   * had executed — an unasked fan-out reported as a completed one.
   */
  it("falls back to a real width when concurrency is not finite", async () => {
    const t = tracker();
    const out = await mapWithConcurrency(12, Number.NaN, (i) => t.run(i));
    expect(out).toEqual(Array.from({ length: 12 }, (_, i) => i));
    expect(t.peak).toBeLessThanOrEqual(DEFAULT_FAN_OUT);
    expect(t.peak).toBeGreaterThan(0);
  });

  it("clamps a zero or negative width to one worker", async () => {
    const t = tracker();
    expect(await mapWithConcurrency(6, 0, (i) => t.run(i))).toHaveLength(6);
    expect(t.peak).toBe(1);
    const t2 = tracker();
    expect(await mapWithConcurrency(6, -5, (i) => t2.run(i))).toHaveLength(6);
    expect(t2.peak).toBe(1);
  });

  it("never spawns more workers than there is work", async () => {
    const t = tracker();
    await mapWithConcurrency(2, 32, (i) => t.run(i));
    expect(t.peak).toBeLessThanOrEqual(2);
  });
});

describe("mapItems", () => {
  it("passes each item and its index, bounded", async () => {
    const t = tracker();
    const items = ["a", "b", "c", "d", "e"];
    const out = await mapItems(items, 2, async (item, index) => {
      await t.run(null);
      return `${index}:${item}`;
    });
    expect(out).toEqual(["0:a", "1:b", "2:c", "3:d", "4:e"]);
    expect(t.peak).toBeLessThanOrEqual(2);
  });

  it("resolves to an empty array for an empty list", async () => {
    const task = vi.fn(async () => 1);
    expect(await mapItems([], 4, task)).toEqual([]);
    expect(task).not.toHaveBeenCalled();
  });
});
