import { describe, expect, it } from "vitest";
import { wallClockBudget } from "../src/lib/testing/wallClock";
import { CHECK_TIMEOUT_MS, ENV_SCRIPT_TIMEOUT_MS, parseEnvLines, spawnBounded } from "./gusset-check.mjs";

describe("gusset:check child processes", () => {
  it("kills a child that outlives its deadline and reports it as an error", () => {
    const started = Date.now();
    const result = spawnBounded("node", ["-e", "setTimeout(() => {}, 60000)"], { stdio: "ignore" }, 300);
    expect((result.error as NodeJS.ErrnoException | undefined)?.code).toBe("ETIMEDOUT");
    expect(Date.now() - started).toBeLessThan(wallClockBudget(10_000));
  });

  it("passes a child that finishes in time through unchanged", () => {
    const result = spawnBounded("node", ["-e", "process.stdout.write('A=1')"], { encoding: "utf8" }, 30_000);
    expect(result.error).toBeUndefined();
    expect(parseEnvLines(String(result.stdout))).toEqual({ A: "1" });
  });

  it("bounds both children with a finite deadline", () => {
    for (const ms of [ENV_SCRIPT_TIMEOUT_MS, CHECK_TIMEOUT_MS]) {
      expect(Number.isFinite(ms)).toBe(true);
      expect(ms).toBeGreaterThan(0);
    }
  });
});
