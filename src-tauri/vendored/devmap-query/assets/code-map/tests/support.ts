/**
 * Test helpers shared by the ported suites. Runner-agnostic: no bun:test import,
 * so the same helpers serve GitPulse's vitest when it runs these files.
 */

/**
 * A wall-clock limit, scaled by `DEVMAP_TEST_TIME_FACTOR` for a loaded or
 * instrumented machine. `wallClockBudget(120)` reads "120 ms on an ordinary run".
 */
export function wallClockBudget(ms: number): number {
  if (!Number.isFinite(ms) || ms < 0) {
    throw new RangeError(`a wall-clock budget must be a finite, non-negative number of ms, got ${ms}`);
  }
  const factor = Number(process.env.DEVMAP_TEST_TIME_FACTOR ?? "1");
  return ms * (Number.isFinite(factor) && factor >= 1 ? factor : 1);
}

/** True when every `keyOf` result is unique: the contract an `{#each}`-style list needs. */
export function eachKeysAreUnique<T>(
  items: readonly T[],
  keyOf: (item: T, index: number) => string,
): boolean {
  const seen = new Set<string>();
  for (let i = 0; i < items.length; i++) {
    const key = keyOf(items[i], i);
    if (seen.has(key)) return false;
    seen.add(key);
  }
  return true;
}
