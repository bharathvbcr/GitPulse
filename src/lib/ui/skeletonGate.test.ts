import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  SKELETON_MIN_HOLD_MS,
  SKELETON_SHOW_DELAY_MS,
  startSkeletonGate,
} from "./skeletonGate";

describe("skeleton gate (fake timers)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("pins the policy numbers", () => {
    expect(SKELETON_SHOW_DELAY_MS).toBe(300);
    expect(SKELETON_MIN_HOLD_MS).toBe(400);
  });

  it("stays hidden for the first 300 ms, then shows once", () => {
    const onShow = vi.fn();
    startSkeletonGate(onShow);
    vi.advanceTimersByTime(299);
    expect(onShow).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(onShow).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(5000);
    expect(onShow).toHaveBeenCalledTimes(1);
  });

  it("never shows a load that finishes inside the delay", () => {
    const onShow = vi.fn();
    const gate = startSkeletonGate(onShow);
    vi.advanceTimersByTime(120);
    gate.cancel();
    vi.advanceTimersByTime(1000);
    expect(onShow).not.toHaveBeenCalled();
    expect(gate.holdRemainingMs()).toBe(0);
  });

  it("holds a shown skeleton for 400 ms, counted from when it appeared", () => {
    const gate = startSkeletonGate(() => {});
    expect(gate.holdRemainingMs()).toBe(0);
    vi.advanceTimersByTime(SKELETON_SHOW_DELAY_MS);
    expect(gate.holdRemainingMs()).toBe(400);
    vi.advanceTimersByTime(150);
    expect(gate.holdRemainingMs()).toBe(250);
    vi.advanceTimersByTime(250);
    expect(gate.holdRemainingMs()).toBe(0);
    vi.advanceTimersByTime(1000);
    expect(gate.holdRemainingMs()).toBe(0);
  });

  it("keeps the hold after cancel, so the leaving skeleton can still read it", () => {
    const gate = startSkeletonGate(() => {});
    vi.advanceTimersByTime(SKELETON_SHOW_DELAY_MS + 100);
    gate.cancel();
    expect(gate.holdRemainingMs()).toBe(300);
  });
});
