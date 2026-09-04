import { expect } from "vitest";

/**
 * Performance budgets that survive a loaded machine.
 *
 * Several suites here assert an absolute wall-clock budget — "10k paths under
 * 5000ms", "20k authors under 2000ms". Those budgets encode the speed of the
 * machine they were written on, so they fail on a busy one for reasons that
 * have nothing to do with the code. Measured: with a concurrent build running
 * (load average 45), ten test files failed purely on timing, and
 * `GraphRendering.rails` took 31,420ms where it takes 2,198ms serially — a 14x
 * spread with an identical tree. Every one of those was a false alarm, and a
 * suite that cries wolf is one people learn to re-run rather than read.
 *
 * Raising the numbers is the wrong fix: it weakens the guard on a fast machine
 * to buy quiet on a slow one. Instead a budget is expressed as a multiple of
 * work this machine can do RIGHT NOW. The reference loop below has a fixed
 * instruction count, so the time it takes is a direct reading of current
 * capacity, load included — and dividing one measurement by the other cancels
 * the machine out of the assertion, leaving the algorithmic claim the test
 * actually means.
 *
 * This is deliberately not a substitute for asserting complexity structurally
 * where that is possible (see markDevParser's bounded-quantifier check). It is
 * for the cases where only a budget will do.
 */

/** Kept out of the optimiser's reach so the loop cannot be elided. */
export let referenceSink = 0;

/**
 * Fixed, deterministic work. Integer-only and allocation-free, so its cost is
 * governed by CPU availability rather than by GC timing or heap state.
 */
function referenceWork(): void {
  let accumulator = 0;
  for (let index = 0; index < 1_000_000; index += 1) {
    accumulator = (accumulator + index * 31) % 1_000_003;
  }
  referenceSink = accumulator;
}

/**
 * Milliseconds this machine currently needs for one reference unit.
 *
 * The median of several samples, not the mean: one descheduled sample would
 * drag a mean upward and quietly inflate every budget derived from it, which
 * is the failure mode this helper exists to remove rather than relocate.
 */
export function referenceUnitMs(samples = 5): number {
  const timings: number[] = [];
  for (let sample = 0; sample < samples; sample += 1) {
    const started = performance.now();
    referenceWork();
    timings.push(performance.now() - started);
  }
  timings.sort((left, right) => left - right);
  return timings[Math.floor(timings.length / 2)];
}

/**
 * Asserts an elapsed time against a budget expressed in reference units.
 *
 * Calibration happens HERE, immediately after the measured work, not before
 * it. That ordering is load-bearing: a budget computed before a slow stretch
 * is exactly as machine-dependent as a hardcoded one, which is how the first
 * version of this helper still failed at load average 107 — it had priced the
 * work at 9.1ms per unit and then run it while the machine was twice as busy.
 *
 * Set `GITPULSE_PERF_REPORT=1` to print the units each case actually consumed,
 * which is how the numbers below were chosen rather than guessed.
 */
export function expectWithinBudget(actualMs: number, units: number, label: string): void {
  const unitMs = referenceUnitMs();
  const allowedMs = units * unitMs;
  if (process.env.GITPULSE_PERF_REPORT) {
    // eslint-disable-next-line no-console
    console.log(
      `PERF ${label}: observed ${(actualMs / unitMs).toFixed(1)} units (budget ${units})`,
    );
  }
  expect(
    actualMs,
    `${label}: took ${actualMs.toFixed(0)}ms against a budget of ${allowedMs.toFixed(0)}ms ` +
      `(${units} reference units at ${unitMs.toFixed(2)}ms each, measured just now)`,
  ).toBeLessThan(allowedMs);
}

/**
 * Timeout for stress and fuzz cases whose assertions are invariants, not speed.
 *
 * Vitest's 5s default is an infrastructure limit, not a guard: these cases
 * assert containment, schema-validity or a rail envelope, and none of them
 * become more true for finishing quickly. Under a concurrent build the same
 * work took 6x longer and seven suites failed on this limit alone, every one a
 * false alarm. Raising it therefore weakens no check — while a genuine
 * non-termination (the defect that this repo actually hit in syntaxHighlight)
 * still fails, just later.
 */
export const STRESS_TIMEOUT_MS = 120_000;
