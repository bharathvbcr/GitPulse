import { readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * A test that bounds elapsed time with a bare number is asserting something
 * about the host it happens to run on. Every limit goes through
 * `wallClockBudget` (`src/lib/testing/wallClock.ts`), which scales it for a
 * coverage run the way `coverage_relaxed` does for the Rust suite. This reads
 * the sources rather than trusting reviewers to notice a new `toBeLessThan(100)`.
 */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function testFiles(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    if (name === "node_modules" || name === "target" || name.startsWith(".")) continue;
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) testFiles(full, out);
    else if (name.endsWith(".test.ts")) out.push(full);
  }
  return out;
}

/** An elapsed-time expression compared against a literal. */
const UNSCALED = [
  /expect\(\s*(?:performance\.now\(\)|Date\.now\(\))\s*-[^)]*\)\.toBeLessThan\(\s*\d/,
  /expect\(\s*(?:elapsed|duration\d?|flattenDuration|windowDuration)\b[^)]*\)\.toBeLessThan\(\s*\d/,
];

describe("wall-clock limits in tests", () => {
  const files = ["src", "scripts", "harness"].flatMap((dir) => testFiles(path.join(ROOT, dir)));
  const self = fileURLToPath(import.meta.url);

  it("finds the test files it is meant to police", () => {
    expect(files.length).toBeGreaterThan(300);
  });

  it("are all written through wallClockBudget", () => {
    const offenders: string[] = [];
    for (const file of files) {
      if (file === self) continue;
      readFileSync(file, "utf8")
        .split("\n")
        .forEach((line, index) => {
          if (UNSCALED.some((pattern) => pattern.test(line))) {
            offenders.push(`${path.relative(ROOT, file)}:${index + 1}: ${line.trim()}`);
          }
        });
    }
    expect(offenders, "wrap the limit as wallClockBudget(<ms>)").toEqual([]);
  });

  it("recognises an unscaled limit when one is written", () => {
    expect(UNSCALED.some((pattern) => pattern.test("expect(performance.now() - started).toBeLessThan(120);"))).toBe(true);
    expect(UNSCALED.some((pattern) => pattern.test("expect(elapsed).toBeLessThan(1_000);"))).toBe(true);
    expect(UNSCALED.some((pattern) => pattern.test("expect(elapsed).toBeLessThan(wallClockBudget(1_000));"))).toBe(false);
  });
});
