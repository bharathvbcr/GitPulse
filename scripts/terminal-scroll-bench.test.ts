import { describe, expect, it } from "vitest";
import { summarize, trialMetrics, quantile } from "./terminal-scroll-bench.mjs";
import { armsFor, gesture, roundOrder } from "../harness/scrollScript";

const trial = (frameMs: number[], steps: number[], extra: Record<string, unknown> = {}) => ({
  kind: "trial", renderer: "dom", input: "wheel", stream: false, smooth: 0, sens: 1, round: 0, frameMs, steps, scrollEvents: 3, handlerMs: 0.1, ...extra,
});

describe("terminal scroll bench metrics", () => {
  it("counts frames over a vsync and over two, and reads p50, p95 and max", () => {
    const frameMs = [...Array.from({ length: 90 }, () => 16.7), ...Array.from({ length: 8 }, () => 20), 40, 90];
    const metrics = trialMetrics(trial(frameMs, frameMs.map(() => 0)) as never);
    expect(metrics).toMatchObject({ p50: 16.7, max: 90, over16: 10, over33: 2, frames: 100 });
    expect(metrics.p95).toBe(20);
  });

  it("does not count a 60 Hz frame's timestamp jitter as a long frame", () => {
    expect(trialMetrics(trial([16.7, 16.8, 17, 18], [0, 0, 0, 0]) as never).over16).toBe(0);
  });

  it("tells a jump from a glide: the largest step and the stalls between the first move and the last", () => {
    // Discrete wheel: 8 rows at once, then frames of nothing.
    const jumpy = trialMetrics(trial([16.7, 16.7, 16.7, 16.7, 16.7, 16.7, 16.7], [0, 8, 0, 0, 0, 8, 0]) as never);
    expect(jumpy).toMatchObject({ maxStep: 8, medStep: 8, stalls: 3, span: 5 });
    // Smoothed: the same distance in small steps with no stall.
    const glide = trialMetrics(trial([16.7, 16.7, 16.7, 16.7, 16.7, 16.7, 16.7], [0, 3, 4, 4, 3, 2, 0]) as never);
    expect(glide).toMatchObject({ maxStep: 4, stalls: 0, span: 5 });
  });

  it("reports a gesture that never moved as no steps rather than a stall", () => {
    expect(trialMetrics(trial([16.7, 16.7], [0, 0]) as never)).toMatchObject({ maxStep: 0, stalls: 0, span: 0 });
  });

  it("takes the quantile of an empty list as NaN, not 0", () => {
    expect(quantile([], 0.5)).toBeNaN();
  });
});

describe("terminal scroll bench summary", () => {
  it("prints min/median over the rounds, one line per arm", () => {
    const lines = summarize([
      trial([16.7, 30, 16.7], [0, 8, 0], { round: 0 }),
      trial([16.7, 50, 16.7], [0, 8, 0], { round: 1 }),
      trial([16.7, 16.7, 16.7], [0, 2, 2], { round: 0, smooth: 150 }),
    ]);
    const unsmoothed = lines.find((line) => line.startsWith("dom/wheel/idle/smooth=0/"));
    const smoothed = lines.find((line) => line.startsWith("dom/wheel/idle/smooth=150/"));
    expect(unsmoothed).toContain(" 2 ");
    expect(unsmoothed).toMatch(/30\.0\/30\.0|30\.0\/50\.0/);
    expect(smoothed).toContain(" 1 ");
    expect(lines.some((line) => line.startsWith("cadence"))).toBe(true);
  });
});

describe("terminal scroll bench harness", () => {
  it("scripts a wheel as 24 identical 100 px notches", () => {
    const events = gesture("wheel");
    expect(events).toHaveLength(24);
    expect(new Set(events.map(([, dy]) => dy))).toEqual(new Set([-100]));
  });

  it("scripts a trackpad at 120 Hz that rises, falls and decays, and rounds to whole pixels for pixel-int", () => {
    const fractional = gesture("pixel");
    const whole = gesture("pixel-int");
    expect(fractional.length).toBeGreaterThan(100);
    expect(fractional[1][0] - fractional[0][0]).toBeCloseTo(1000 / 120, 5);
    expect(fractional.every(([, dy]) => dy < 0)).toBe(true);
    expect(fractional.some(([, dy]) => !Number.isInteger(dy))).toBe(true);
    expect(whole.every(([, dy]) => Number.isInteger(dy))).toBe(true);
    expect(Math.abs(fractional.at(-1)![1])).toBeLessThan(1);
  });

  it("rotates the arm order each round so every arm leads once and none is always last", () => {
    const arms = armsFor({ renderers: ["webgl", "dom"], inputs: ["wheel"], streams: [false, true], smooths: [0, 150], sens: [1], guards: [false, true] });
    expect(arms).toHaveLength(12);
    expect(arms.some((arm) => arm.guard && arm.smooth === 0)).toBe(false);
    const leaders = new Set<string>();
    const trailers = new Set<string>();
    for (let round = 0; round < arms.length; round += 1) {
      const order = roundOrder(arms, round);
      expect(order.map((arm) => JSON.stringify(arm)).sort()).toEqual(arms.map((arm) => JSON.stringify(arm)).sort());
      leaders.add(JSON.stringify(order[0]));
      trailers.add(JSON.stringify(order.at(-1)));
    }
    expect(leaders.size).toBeGreaterThan(1);
    expect(trailers.size).toBeGreaterThan(1);
  });
});
