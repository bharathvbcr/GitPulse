/**
 * Adversarial stress for the catastrophic-backtracking guard.
 *
 * The guard is a static approximation, so the property worth testing is not
 * "every dangerous pattern is named" — no static check can promise that — but
 * the two that are actually falsifiable:
 *
 *   1. every pattern the guard ADMITS runs fast on hostile input, and
 *   2. the guard has not become so strict that ordinary filters stop working.
 *
 * Both holes this file now pins were found by fuzzing rather than by reading:
 * ambiguous alternation (`(a|a)*`, 6.3 s at 28 characters) and bounded
 * repetition (`(?:\w|(\w+){2,4}){3}`, 5.0 s at 26, tripling every two), which
 * the original rule admitted because neither `{3}` nor `{2,4}` registered as
 * a quantifier at all.
 *
 * Budgets here are deliberately loose. A tight wall-clock assertion on a
 * machine running concurrent builds reports load, not a regression; the gap
 * between an admitted pattern (sub-millisecond) and a catastrophic one
 * (seconds) is three orders of magnitude, so a generous ceiling still fails
 * loudly on a real one. Inputs are capped at 40 characters for the same
 * reason: a missed pattern must surface as a failed assertion, never as a
 * test run that hangs.
 */
import { describe, expect, it } from "vitest";
import { hasUnboundedNesting } from "./lineSearch";

/** Deterministic PRNG, so a failure is reproducible from its seed alone. */
function rng(seed: number): () => number {
  let state = seed >>> 0;
  return () => (state = (state * 1664525 + 1013904223) >>> 0) / 2 ** 32;
}

const ATOMS = ["a", "b", "ab", "aa", "[a-z]", "[a-b]", "[0-9]", "\\d", "\\w", "\\s", ".", "x", "foo", "bar", "_"];
const QUANTS = ["*", "+", "{1,}", "{2,}", "{2,4}", "{3}", "{1}", "?", ""];

function generatePattern(next: () => number): string {
  const pick = <T,>(xs: readonly T[]): T => xs[Math.floor(next() * xs.length)];
  const branches: string[] = [];
  for (let b = 0; b < 1 + Math.floor(next() * 4); b += 1) {
    let piece = "";
    for (let k = 0; k < 1 + Math.floor(next() * 3); k += 1) {
      piece +=
        next() < 0.18
          ? `(${next() < 0.4 ? "?:" : ""}${pick(ATOMS)}${next() < 0.6 ? pick(QUANTS) : ""})`
          : pick(ATOMS);
      if (next() < 0.45) piece += pick(QUANTS);
    }
    branches.push(piece);
  }
  const body = branches.join("|");
  const group = next() < 0.3 ? `(?:${body})` : `(${body})`;
  return `${group}${pick(QUANTS)}${next() < 0.75 ? "$" : ""}`;
}

/** Bases chosen so a repeated run ends in a character the pattern cannot match. */
const HOSTILE_BASES = ["a", "ab", "foo", "a b ", "0123456789", "x"];

/**
 * Runs `pattern` against escalating hostile inputs, stopping at the first
 * length that exceeds `budgetMs`. Escalation is what keeps a catastrophic
 * pattern from hanging the suite: it is caught at a short length and
 * abandoned before the exponential reaches a length that would not return.
 */
function slowestHostileRun(pattern: string, budgetMs: number): { ms: number; length: number } {
  const matcher = new RegExp(pattern);
  let worst = { ms: 0, length: 0 };
  for (const base of HOSTILE_BASES) {
    for (let length = 8; length <= 40; length += 4) {
      const input = base.repeat(Math.ceil(length / base.length)).slice(0, length) + "!";
      const started = performance.now();
      matcher.test(input);
      const elapsed = performance.now() - started;
      if (elapsed > worst.ms) worst = { ms: elapsed, length };
      if (elapsed > budgetMs) return worst;
    }
  }
  return worst;
}

describe("catastrophic-backtracking guard under fuzz", () => {
  it("admits nothing that blows up on hostile input", () => {
    const next = rng(20260905);
    const budgetMs = 250;
    const offenders: string[] = [];
    let admitted = 0;
    for (let i = 0; i < 3_000; i += 1) {
      const pattern = generatePattern(next);
      try {
        new RegExp(pattern);
      } catch {
        continue; // Generated garbage is the generator's problem, not the guard's.
      }
      if (hasUnboundedNesting(pattern)) continue;
      admitted += 1;
      const worst = slowestHostileRun(pattern, budgetMs);
      if (worst.ms > budgetMs) {
        offenders.push(`${pattern} took ${worst.ms.toFixed(0)}ms at length ${worst.length}`);
      }
    }
    // A guard that refused everything would pass the assertion above while
    // making the feature useless, so the sample size is asserted too.
    expect(admitted).toBeGreaterThan(500);
    expect(offenders, `admitted patterns exceeded ${budgetMs}ms`).toEqual([]);
  });

  it("refuses every family measured to blow up", () => {
    for (const pattern of [
      "(a+)+$", // nested quantifier
      "([a-z]+\\s*)+$",
      "(a|a)*$", // ambiguous alternation
      "(?:aa|a)*$",
      "([a-z]|[a-z][a-z])*$",
      "(aa?)*$", // variable-length body
      "([a-z]\\w?)*$",
      "(?:\\w|(\\w+){2,4}){3}$", // bounded repetition
      "(\\w+){2,4}$",
      "(a+){2}$",
    ]) {
      expect(hasUnboundedNesting(pattern), pattern).toBe(true);
    }
  });

  it("keeps the filter language usable for patterns people actually type", () => {
    for (const pattern of [
      "\\.ts$",
      "^src/",
      "main\\.ts$",
      "(foo|bar)",
      "\\.(ts|js)$",
      "components?/",
      "[A-Z]\\w+\\.svelte$",
      "test|spec",
      "^(?:src|lib)/.*\\.rs$",
      "index\\.",
      "\\d{4}-\\d{2}",
      ".*\\.json$",
      "util",
    ]) {
      expect(hasUnboundedNesting(pattern), pattern).toBe(false);
      expect(slowestHostileRun(pattern, 250).ms, pattern).toBeLessThan(250);
    }
  });
});
