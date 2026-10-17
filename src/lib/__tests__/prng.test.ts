import { describe, expect, it } from "vitest";
import { mulberry32, mulberry32Uint32 } from "./prng";

/** The integer-returning generator `repos/tabModel.test.ts` carried before it
 * shared this one, kept verbatim as the oracle for the shared integer stream. */
function integerOracle(seed: number): () => number {
  let t = seed >>> 0;
  return () => {
    t += 0x6d2b79f5;
    let r = Math.imul(t ^ (t >>> 15), 1 | t);
    r ^= r + Math.imul(r ^ (r >>> 7), 61 | r);
    return ((r ^ (r >>> 14)) >>> 0) % 0x100000000;
  };
}

describe("shared test PRNG", () => {
  it("floats stay in [0, 1) and repeat for a seed", () => {
    const a = mulberry32(0x51f1e7);
    const b = mulberry32(0x51f1e7);
    for (let i = 0; i < 1_000; i += 1) {
      const value = a();
      expect(value).toBe(b());
      expect(value).toBeGreaterThanOrEqual(0);
      expect(value).toBeLessThan(1);
    }
  });

  it("the unsigned stream is exactly the integer stream tabModel used", () => {
    for (const seed of [0, 1, 0x51f1e7, 0xffffffff, -5]) {
      const shared = mulberry32Uint32(seed);
      const oracle = integerOracle(seed);
      const got: number[] = [];
      const want: number[] = [];
      for (let i = 0; i < 10_000; i += 1) {
        got.push(shared());
        want.push(oracle());
      }
      expect(got.every(Number.isInteger)).toBe(true);
      expect(got).toEqual(want);
    }
  });
});
