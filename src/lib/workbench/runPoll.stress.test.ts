import { describe, expect, it } from "vitest";
import { CADENCE_CEILING_MS, LAG_HARD_MS } from "../runtime/loadCadence";
import { TASK_RUN_POLL_MS, nextTaskRunPollDelay, type TaskRunPollInput } from "./runPoll";

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

describe("nextTaskRunPollDelay", () => {
  it("refuses a poll that is background, idle, or not on screen", () => {
    const live = { baseMs: TASK_RUN_POLL_MS, lagMs: 0, background: false, live: true, active: true };
    expect(nextTaskRunPollDelay(live)).toBe(TASK_RUN_POLL_MS);
    expect(nextTaskRunPollDelay({ ...live, background: true })).toBeNull();
    expect(nextTaskRunPollDelay({ ...live, live: false })).toBeNull();
    expect(nextTaskRunPollDelay({ ...live, active: false })).toBeNull();
    for (const flag of [undefined, null, 0, 1, "true"] as const) {
      expect(nextTaskRunPollDelay({ ...live, background: flag as unknown as boolean })).toBeNull();
      expect(nextTaskRunPollDelay({ ...live, live: flag as unknown as boolean })).toBeNull();
      expect(nextTaskRunPollDelay({ ...live, active: flag as unknown as boolean })).toBeNull();
    }
  });

  it("stretches a live run when the event loop is hard-late and refuses a spin", () => {
    expect(nextTaskRunPollDelay({
      baseMs: TASK_RUN_POLL_MS,
      lagMs: LAG_HARD_MS + 50,
      background: false,
      live: true,
      active: true,
    })).toBe(TASK_RUN_POLL_MS * 4);
    expect(nextTaskRunPollDelay({
      baseMs: 0,
      lagMs: 0,
      background: false,
      live: true,
      active: true,
    })).toBeNull();
  });

  it("stays null or finite across 20,000 hostile inputs", () => {
    const rand = mulberry32(0xca5e11ce);
    const hostile = [0, -1, 1, 250, 800, 30_000, Number.NaN, Number.POSITIVE_INFINITY, Number.MAX_SAFE_INTEGER];
    const flags = [true, false, undefined, null, 0, 1, "yes"];
    for (let i = 0; i < 20_000; i += 1) {
      const pick = <T>(values: readonly T[]): T => values[Math.floor(rand() * values.length)] as T;
      const input: TaskRunPollInput = {
        baseMs: rand() < 0.2 ? pick(hostile) : Math.floor(rand() * 200_000),
        lagMs: rand() < 0.3 ? pick(hostile) : rand() * 80_000 - 1_000,
        background: pick(flags) as boolean,
        live: pick(flags) as boolean,
        active: pick(flags) as boolean,
      };
      let delay: number | null = 0;
      expect(() => { delay = nextTaskRunPollDelay(input); }, `step ${i}`).not.toThrow();
      const running = input.active === true && input.background === false && input.live === true;
      if (!running) {
        expect(delay, `refused step ${i}`).toBeNull();
        continue;
      }
      if (delay === null) continue;
      expect(Number.isFinite(delay), `step ${i}`).toBe(true);
      expect(delay, `step ${i}`).toBeGreaterThan(0);
      expect(delay, `step ${i}`).toBeLessThanOrEqual(Math.max(input.baseMs, CADENCE_CEILING_MS));
      if (input.baseMs <= CADENCE_CEILING_MS) expect(delay, `step ${i}`).toBeGreaterThanOrEqual(input.baseMs);
    }
  });
});
