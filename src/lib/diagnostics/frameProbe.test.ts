import { describe, expect, it } from "vitest";
import { isReaderInput } from "../terminal/sessionActivity";
import { createFrameProbe, FRAME_PROBE_OSC, frameStats, handleFrameProbeCommand } from "./frameProbe";

/** A rAF that runs only when the test advances it, at the times it gives. */
function frames() {
  let pending: ((time: number) => void) | null = null;
  let cancelled = 0;
  return {
    raf: (callback: (time: number) => void) => { pending = callback; return 1; },
    cancel: () => { pending = null; cancelled += 1; },
    at(...times: number[]) {
      for (const time of times) {
        const run = pending;
        pending = null;
        run?.(time);
      }
    },
    waiting: () => pending !== null,
    cancelled: () => cancelled,
  };
}

describe("frame stats", () => {
  it("counts intervals longer than 1.5x the period as dropped", () => {
    const stats = frameStats([16.7, 16.6, 33.4, 16.7, 50.1], 16.67);
    expect(stats).toMatchObject({ intervals: 5, dropped: 2, fraction: 0.4, periodMs: 16.67 });
    expect(stats?.p95Ms).toBe(50.1);
  });

  it("falls back to the median when no period is given or it is unusable", () => {
    expect(frameStats([8, 8, 8, 30])?.periodMs).toBe(8);
    expect(frameStats([8, 8, 8, 30], 0)?.periodMs).toBe(8);
    expect(frameStats([8, 8, 8, 30], Number.NaN)?.dropped).toBe(1);
  });

  it("has nothing to say about no frames", () => {
    expect(frameStats([])).toBeNull();
  });
});

describe("frame probe", () => {
  it("records the interval between successive frames until stopped", () => {
    const clock = frames();
    const probe = createFrameProbe(clock);
    probe.start(16);
    clock.at(100, 116, 132, 164);
    expect(probe.running()).toBe(true);
    const stats = probe.stop();
    expect(stats).toMatchObject({ intervals: 3, dropped: 1, periodMs: 16, recordedMs: 64 });
    expect(probe.running()).toBe(false);
    expect(clock.waiting()).toBe(false);
  });

  it("starts afresh, discarding an earlier run", () => {
    const clock = frames();
    const probe = createFrameProbe(clock);
    probe.start();
    clock.at(0, 50, 100);
    probe.start();
    clock.at(200, 210);
    expect(probe.stop()?.intervals).toBe(1);
  });

  it("stops itself at its time limit and keeps what it recorded", () => {
    const clock = frames();
    const probe = createFrameProbe({ ...clock, maxMs: 100 });
    probe.start();
    clock.at(0, 50, 100, 150);
    expect(probe.running()).toBe(false);
    expect(probe.stop()?.intervals).toBe(2);
  });
});

describe("OSC 7700 frame commands", () => {
  it("answers a stop with the stats, as a terminal reply", () => {
    const clock = frames();
    const probe = createFrameProbe(clock);
    expect(handleFrameProbeCommand(probe, "frames-start;period=16.67")).toBeNull();
    clock.at(0, 16.67, 33.34, 66.68);
    const reply = handleFrameProbeCommand(probe, "frames-stop");
    expect(reply).toBe(`\u001b]${FRAME_PROBE_OSC};frames;n=3;dropped=1;fraction=0.3333;period=16.67;p50=16.67;p95=33.34;ms=67\u0007`);
    // The reply is xterm speaking, not the reader: it must not clear an
    // agent's waiting banner the way a typed answer does.
    expect(isReaderInput(reply ?? "")).toBe(false);
  });

  it("answers a stop with no frames as n=0", () => {
    const probe = createFrameProbe(frames());
    expect(handleFrameProbeCommand(probe, "frames-stop")).toBe(`\u001b]${FRAME_PROBE_OSC};frames;n=0\u0007`);
  });

  it("ignores an unusable period and unknown commands", () => {
    const clock = frames();
    const probe = createFrameProbe(clock);
    handleFrameProbeCommand(probe, "frames-start;period=-3");
    clock.at(0, 10, 10.5, 30);
    expect(handleFrameProbeCommand(probe, "frames-stop")).toContain("period=10;");
    expect(handleFrameProbeCommand(probe, "anything-else")).toBeNull();
    expect(handleFrameProbeCommand(probe, "")).toBeNull();
  });
});
