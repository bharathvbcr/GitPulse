import { describe, expect, it } from "vitest";
import { createWatchPool, WATCH_POOL_SIZE } from "./watchPool";

describe("createWatchPool", () => {
  it("refuses a pool with no slots", () => {
    for (const bad of [0, -1, 1.5, Number.NaN]) {
      expect(() => createWatchPool(bad)).toThrow(RangeError);
    }
  });

  it("admits up to capacity in the background, then parks", () => {
    const pool = createWatchPool(3);
    for (const key of ["a", "b", "c"]) expect(pool.admit(key, false)).toEqual({ admitted: true, evicted: null });
    expect(pool.admit("d", false)).toEqual({ admitted: false, evicted: null });
    expect(pool.keys()).toEqual(["a", "b", "c"]);
  });

  it("evicts the least recently used key for a foreground admission", () => {
    const pool = createWatchPool(3);
    for (const key of ["a", "b", "c"]) pool.admit(key, false);
    pool.admit("a", true); // a is now the most recent
    expect(pool.admit("d", true)).toEqual({ admitted: true, evicted: "b" });
    expect(pool.keys()).toEqual(["c", "a", "d"]);
  });

  it("re-admitting a held key moves it without evicting", () => {
    const pool = createWatchPool(2);
    pool.admit("a", false);
    pool.admit("b", false);
    expect(pool.admit("a", false)).toEqual({ admitted: true, evicted: null });
    expect(pool.keys()).toEqual(["b", "a"]);
  });

  it("frees a slot on release, and says when there was none", () => {
    const pool = createWatchPool(1);
    pool.admit("a", false);
    expect(pool.release("a")).toBe(true);
    expect(pool.release("a")).toBe(false);
    expect(pool.admit("b", false).admitted).toBe(true);
  });

  it("never admits an empty key", () => {
    const pool = createWatchPool(1);
    expect(pool.admit("", true)).toEqual({ admitted: false, evicted: null });
    expect(pool.size).toBe(0);
  });

  it("holds its invariants under a long random workload", () => {
    // A small deterministic generator, so a failure replays.
    let seed = 0x2f6b;
    const next = () => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed / 0x7fffffff;
    };
    const pool = createWatchPool(WATCH_POOL_SIZE);
    const model: string[] = []; // least recently used first
    for (let step = 0; step < 20_000; step += 1) {
      const key = `repo-${Math.floor(next() * 300)}`;
      const roll = next();
      if (roll < 0.15) {
        const had = model.includes(key);
        expect(pool.release(key)).toBe(had);
        if (had) model.splice(model.indexOf(key), 1);
      } else {
        const foreground = roll < 0.55;
        const outcome = pool.admit(key, foreground);
        const had = model.includes(key);
        if (had) {
          expect(outcome).toEqual({ admitted: true, evicted: null });
          model.splice(model.indexOf(key), 1);
          model.push(key);
        } else if (model.length < WATCH_POOL_SIZE) {
          expect(outcome).toEqual({ admitted: true, evicted: null });
          model.push(key);
        } else if (foreground) {
          // The foreground key is always admitted, and only by the oldest.
          expect(outcome).toEqual({ admitted: true, evicted: model[0] });
          model.shift();
          model.push(key);
        } else {
          expect(outcome).toEqual({ admitted: false, evicted: null });
        }
      }
      expect(pool.size).toBeLessThanOrEqual(WATCH_POOL_SIZE);
      expect(pool.keys()).toEqual(model);
    }
  });
});
