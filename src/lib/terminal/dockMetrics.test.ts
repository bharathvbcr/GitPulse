import { describe, expect, it } from "vitest";
import {
  TERMINAL_DOCK_DEFAULT_HEIGHT,
  TERMINAL_DOCK_MAX_HEIGHT,
  TERMINAL_DOCK_MIN_HEIGHT,
  clampTerminalDockHeight,
  fitTerminalDockHeight,
} from "./dockMetrics";

describe("clampTerminalDockHeight", () => {
  it("keeps a sensible height unchanged", () => {
    expect(clampTerminalDockHeight(300)).toBe(300);
  });

  it("holds the dock inside its range", () => {
    expect(clampTerminalDockHeight(10)).toBe(TERMINAL_DOCK_MIN_HEIGHT);
    expect(clampTerminalDockHeight(10_000)).toBe(TERMINAL_DOCK_MAX_HEIGHT);
  });

  it("falls back to the default on a non-finite height", () => {
    // A persisted NaN would otherwise render a zero-pixel dock: present in
    // the DOM, invisible on screen, and with no separator left to grab.
    expect(clampTerminalDockHeight(Number.NaN)).toBe(TERMINAL_DOCK_DEFAULT_HEIGHT);
    expect(clampTerminalDockHeight(Number.POSITIVE_INFINITY)).toBe(
      TERMINAL_DOCK_DEFAULT_HEIGHT,
    );
  });

  it("snaps to whole pixels so sub-pixel drags cannot shimmer the layout", () => {
    expect(clampTerminalDockHeight(300.4)).toBe(300);
    expect(clampTerminalDockHeight(300.6)).toBe(301);
  });
});

describe("fitTerminalDockHeight", () => {
  it("honours the request when the window has room for it", () => {
    expect(fitTerminalDockHeight(300, 900)).toBe(300);
  });

  it("never lets the dock take the whole column", () => {
    // A height stored on a large display must not swallow the view when the
    // same preference is restored on a small one.
    expect(fitTerminalDockHeight(800, 600, 160)).toBe(440);
  });

  it("keeps the dock grabbable even in a window too short for both", () => {
    // Below the point where view and dock both fit, the minimum wins: a dock
    // shrunk to nothing is one the user cannot drag back.
    expect(fitTerminalDockHeight(400, 200, 160)).toBe(TERMINAL_DOCK_MIN_HEIGHT);
  });

  it("returns the clamped request before the container has been measured", () => {
    // ResizeObserver reports nothing until the first frame; a zero height
    // there means "unknown", not "no room", and must not collapse the dock.
    expect(fitTerminalDockHeight(300, 0)).toBe(300);
    expect(fitTerminalDockHeight(10_000, 0)).toBe(TERMINAL_DOCK_MAX_HEIGHT);
  });
});
