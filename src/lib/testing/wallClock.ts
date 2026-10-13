/**
 * The one place a test's wall-clock limit is scaled for the machine it runs on.
 *
 * A bare `toBeLessThan(120)` on elapsed time is a claim about the host, not the
 * code: coverage instrumentation slows every function, and a loaded runner
 * slows the rest. `vitest.config.ts` marks a coverage run through
 * `GITPULSE_COVERAGE_RUN`; under it a limit is multiplied (and floored at five
 * seconds) the way `coverage_relaxed` does it for the Rust tests, so the two
 * suites scale one way. The limit a test *means* stays the number it writes:
 * `wallClockBudget(120)` reads as "120 ms on an ordinary run".
 *
 * Prefer counting the work (calls made, rows visited) over timing it; use this
 * for the cases where elapsed time is the property under test.
 * `scripts/wall-clock-contract.test.ts` refuses a new unscaled limit.
 */

/** Factor and floor applied under coverage; mirrors `coverage_relaxed` in Rust. */
export const COVERAGE_FACTOR = 8;
export const COVERAGE_FLOOR_MS = 5_000;

export function isCoverageRun(env: Record<string, string | undefined> = process.env): boolean {
  const flag = env.GITPULSE_COVERAGE_RUN;
  return flag !== undefined && flag !== "" && flag !== "0";
}

/** `ms` on an ordinary run; scaled up under coverage instrumentation. */
export function wallClockBudget(
  ms: number,
  env: Record<string, string | undefined> = process.env,
): number {
  if (!Number.isFinite(ms) || ms < 0) throw new RangeError(`a wall-clock budget must be a finite, non-negative number of ms, got ${ms}`);
  return isCoverageRun(env) ? Math.max(ms * COVERAGE_FACTOR, COVERAGE_FLOOR_MS) : ms;
}
