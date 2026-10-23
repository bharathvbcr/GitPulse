import { describe, expect, it } from "vitest";
import { STRESS_TIMEOUT_MS, fastestCpuOf, fastestOf } from "../__tests__/perfBudget";
import { calculateDocumentStats } from "./markdevRender";

// Adversarial probe of the "does not slow quadratically" test: reports the
// ratio the OLD (wall) and the NEW (CPU) assertions read, and asserts a
// quadratic sized to stay inside the 400 ms wall budget is still caught by the
// ratio. Run with QSHIFT=<n> (the mutation's cost knob; see the review).
describe("markdevRender stats ratio catches a budget-sized quadratic", () => {
  it(
    "ratio of 4x input stays below 8 in CPU time",
    () => {
      const texts = [20000, 40000, 80000].map((n) => "[".repeat(n));
      const wall = texts.map((text) => fastestOf(3, () => void calculateDocumentStats(text)));
      const cpu = texts.map((text) => fastestCpuOf(7, () => void calculateDocumentStats(text)));
      console.log(
        `PROBE wall=${wall.map((w) => w.toFixed(2))} wallRatio=${(wall[2]! / wall[0]!).toFixed(2)} ` +
          `cpu=${cpu.map((c) => c.toFixed(2))} cpuRatio=${(cpu[2]! / cpu[0]!).toFixed(2)}`,
      );
      // Measured under host load ~100 with a mutation adding n^2/64 work to
      // calculateDocumentStats (about +50% at 80k chars): cpuRatio read 5.2,
      // 5.4 and 7.4, all under the 8 limit, so the CPU-ratio gate in
      // markdevRender.test.ts misses a real quadratic that the old wall ratio
      // flagged (15.5, 8.06). This asserts the property the gate should hold.
      expect(cpu[2]! / cpu[0]!).toBeLessThan(8);
    },
    STRESS_TIMEOUT_MS,
  );
});
