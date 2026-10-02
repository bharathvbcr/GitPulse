import { afterEach, describe, expect, it } from "vitest";
import {
  CADENCE_CEILING_MS,
  LAG_HARD_MS,
  LAG_STRETCH_MS,
  SUSPEND_GAP_MS,
  decideCadence,
  noteEventLoopDelay,
  readEventLoopDelay,
  resetEventLoopDelay,
  type CadenceInput,
} from "./loadCadence";

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function assertClosed(decision: ReturnType<typeof decideCadence>, label: string): void {
  expect(decision.reason.trim(), label).not.toBe("");
  expect(Number.isFinite(decision.delayMs), label).toBe(true);
  expect(decision.delayMs, label).toBeGreaterThanOrEqual(0);
  if (decision.run) {
    expect(decision.delayMs, label).toBeGreaterThan(0);
  } else {
    expect(decision.delayMs, label).toBe(0);
  }
}

describe("decideCadence", () => {
  it("refuses intervals that would spin", () => {
    for (const baseMs of [0, -1, -0, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
      const decision = decideCadence({ baseMs, lagMs: 0, paused: false });
      assertClosed(decision, String(baseMs));
      expect(decision.run).toBe(false);
      expect(decision.reason).toContain("non-positive");
    }
  });

  it("treats a missing pause flag as paused", () => {
    const decision = decideCadence({ baseMs: 1_000, lagMs: 0, paused: undefined as unknown as boolean });
    expect(decision.run).toBe(false);
    expect(decision.delayMs).toBe(0);
    expect(decision.reason).toContain("not a boolean");
  });

  it("pauses without leaving a delay that a caller could still schedule", () => {
    const decision = decideCadence({ baseMs: 6_000, lagMs: 10_000, paused: true });
    expect(decision).toMatchObject({ run: false, delayMs: 0 });
  });

  it("keeps the base period while the loop is inside the quiet band", () => {
    expect(decideCadence({ baseMs: 2_000, lagMs: 0, paused: false }).delayMs).toBe(2_000);
    expect(decideCadence({ baseMs: 2_000, lagMs: LAG_STRETCH_MS - 1, paused: false }).delayMs).toBe(2_000);
  });

  it("stretches once at the late threshold and again when the loop is hard-late", () => {
    expect(decideCadence({ baseMs: 2_000, lagMs: LAG_STRETCH_MS, paused: false })).toMatchObject({
      run: true,
      delayMs: 4_000,
      reason: "event loop late",
    });
    expect(decideCadence({ baseMs: 2_000, lagMs: LAG_HARD_MS - 1, paused: false }).delayMs).toBe(4_000);
    expect(decideCadence({ baseMs: 2_000, lagMs: LAG_HARD_MS, paused: false })).toMatchObject({
      delayMs: 8_000,
      reason: "event loop hard-late",
    });
  });

  it("does not treat a sleep-sized gap as load", () => {
    const decision = decideCadence({ baseMs: 6_000, lagMs: SUSPEND_GAP_MS, paused: false });
    expect(decision).toMatchObject({ run: true, delayMs: 6_000, reason: "suspend gap is not load" });
  });

  it("treats a broken lag reading as pressure rather than as a quiet machine", () => {
    for (const lagMs of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY, -1]) {
      const decision = decideCadence({ baseMs: 1_000, lagMs, paused: false });
      assertClosed(decision, String(lagMs));
      expect(decision.run).toBe(true);
      expect(decision.delayMs).toBe(4_000);
      expect(decision.reason).toContain("non-finite");
    }
  });

  it("caps the stretch so a long base cannot overflow into a non-finite delay", () => {
    const decision = decideCadence({
      baseMs: Number.MAX_SAFE_INTEGER,
      lagMs: LAG_HARD_MS,
      paused: false,
    });
    assertClosed(decision, "overflow");
    expect(decision.delayMs).toBeLessThanOrEqual(CADENCE_CEILING_MS);
  });

  it("never schedules faster than the caller asked, and never past the caller's ceiling", () => {
    const decision = decideCadence({
      baseMs: 5_000,
      maxMs: 7_000,
      lagMs: LAG_HARD_MS,
      paused: false,
    });
    expect(decision.delayMs).toBe(7_000);
  });

  it("holds the invariants across a seeded storm of hostile inputs", () => {
    const rand = mulberry32(0xca5e11ce);
    const hostile = [
      0, -1, 1, 16, 250, 750, 30_000, Number.NaN, Number.POSITIVE_INFINITY,
      Number.NEGATIVE_INFINITY, Number.MAX_SAFE_INTEGER, Number.MIN_SAFE_INTEGER,
    ];
    for (let i = 0; i < 20_000; i += 1) {
      const pick = (n: number) => hostile[Math.floor(rand() * hostile.length)] ?? n;
      const input: CadenceInput = {
        baseMs: rand() < 0.2 ? pick(0) : Math.floor(rand() * 100_000),
        maxMs: rand() < 0.3 ? pick(0) : Math.floor(rand() * 200_000),
        lagMs: rand() < 0.3 ? pick(0) : rand() * 80_000 - 1_000,
        paused: rand() < 0.15 ? (undefined as unknown as boolean) : rand() < 0.4,
      };
      assertClosed(decideCadence(input), `seed 0xca5e11ce step ${i}`);
    }
  });
});

describe("event-loop sample", () => {
  afterEach(() => resetEventLoopDelay());

  it("rises on the sample that observed the stall", () => {
    noteEventLoopDelay(0);
    noteEventLoopDelay(LAG_HARD_MS);
    expect(readEventLoopDelay()).toBe(LAG_HARD_MS);
  });

  it("decays halfway per healthy sample instead of snapping back", () => {
    noteEventLoopDelay(800);
    noteEventLoopDelay(0);
    expect(readEventLoopDelay()).toBe(400);
    noteEventLoopDelay(0);
    expect(readEventLoopDelay()).toBe(200);
  });

  it("clears on a suspend-sized gap and treats a bad sample as hard pressure", () => {
    noteEventLoopDelay(800);
    noteEventLoopDelay(SUSPEND_GAP_MS);
    expect(readEventLoopDelay()).toBe(0);
    noteEventLoopDelay(Number.NaN);
    expect(readEventLoopDelay()).toBe(LAG_HARD_MS);
  });

  it("stays finite across a long run of mixed samples", () => {
    const rand = mulberry32(0x591e);
    for (let i = 0; i < 50_000; i += 1) {
      const roll = rand();
      const ms = roll < 0.05 ? Number.NaN : roll < 0.1 ? -5 : roll < 0.15 ? 60_000 : roll * 2_000;
      noteEventLoopDelay(ms);
      const sample = readEventLoopDelay();
      expect(Number.isFinite(sample), `step ${i}`).toBe(true);
      expect(sample).toBeGreaterThanOrEqual(0);
      expect(sample).toBeLessThanOrEqual(SUSPEND_GAP_MS);
    }
  });
});
