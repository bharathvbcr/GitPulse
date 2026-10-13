/**
 * A seeded pseudo-random generator for property-style suites.
 *
 * Randomised tests need a source that is random across cases but identical
 * across runs: a failure nobody can reproduce is a failure nobody fixes.
 * `Math.random()` cannot do that, so suites reach for mulberry32 — and it had
 * been pasted verbatim into two of them (branches/menuPosition, branches/pins).
 * One copy is enough, and it keeps the seeds comparable between suites.
 */
export function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * The same stream as {@link mulberry32} as unsigned 32-bit integers, for
 * suites that pick with `rng() % n`. Exactly the integer the generator
 * produced before the float division (the quotient by 2^32 is exact in a
 * double, so the product recovers it), which keeps a seed's walk identical.
 */
export function mulberry32Uint32(seed: number): () => number {
  const next = mulberry32(seed);
  return () => next() * 4294967296;
}
