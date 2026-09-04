import { describe, expect, it } from "vitest";
import {
  STATUS_BAR_MODES,
  isStatusBarMode,
  resolveStatusBarMode,
  type StatusBarSignals,
} from "./statusBarMode";

const quiet: StatusBarSignals = {
  operationParked: false,
  conflictedCount: 0,
  watchDegraded: false,
};

describe("isStatusBarMode", () => {
  it("accepts each mode and rejects everything else", () => {
    for (const mode of STATUS_BAR_MODES) expect(isStatusBarMode(mode)).toBe(true);
    for (const value of ["", "off", "FULL", 1, null, undefined, {}, ["full"]]) {
      expect(isStatusBarMode(value), `value: ${JSON.stringify(value)}`).toBe(false);
    }
  });
});

describe("resolveStatusBarMode", () => {
  it("honours full and compact untouched, quiet or not", () => {
    for (const mode of ["full", "minimal"] as const) {
      expect(resolveStatusBarMode(mode, quiet)).toEqual({ mode, forced: false });
      expect(
        resolveStatusBarMode(mode, { ...quiet, conflictedCount: 3 }),
      ).toEqual({ mode, forced: false });
    }
  });

  it("stays hidden while the repository is quiet", () => {
    expect(resolveStatusBarMode("hidden", quiet)).toEqual({ mode: "hidden", forced: false });
  });

  it.each([
    ["a parked operation", { ...quiet, operationParked: true }],
    ["unresolved conflicts", { ...quiet, conflictedCount: 1 }],
    ["a degraded watcher", { ...quiet, watchDegraded: true }],
  ])("forces a hidden bar back for %s", (_label, signals) => {
    // Hiding chrome is allowed to cost the ambient readouts. It is not
    // allowed to cost a warning: silence here is how a user finds out too
    // late that a merge was parked or the screen stopped refreshing.
    expect(resolveStatusBarMode("hidden", signals)).toEqual({ mode: "minimal", forced: true });
  });

  it("does not force on a negative or absent count", () => {
    expect(resolveStatusBarMode("hidden", { ...quiet, conflictedCount: 0 }).forced).toBe(false);
  });
});
