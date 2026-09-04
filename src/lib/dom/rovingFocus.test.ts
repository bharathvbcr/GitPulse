import { describe, expect, it } from "vitest";
import { nextRovingIndex } from "./rovingFocus";

describe("nextRovingIndex", () => {
  it("moves within the list and wraps at both ends", () => {
    expect(nextRovingIndex(0, 5, "ArrowRight")).toBe(1);
    expect(nextRovingIndex(4, 5, "ArrowRight")).toBe(0);
    expect(nextRovingIndex(3, 5, "ArrowLeft")).toBe(2);
    expect(nextRovingIndex(0, 5, "ArrowLeft")).toBe(4);
  });

  it("jumps to the edges on Home/End", () => {
    expect(nextRovingIndex(2, 5, "Home")).toBe(0);
    expect(nextRovingIndex(2, 5, "End")).toBe(4);
  });

  it("enters the list in travel direction when focus is on the container", () => {
    // current -1 = focus sits on the tablist itself.
    expect(nextRovingIndex(-1, 5, "ArrowRight")).toBe(0);
    expect(nextRovingIndex(-1, 5, "Home")).toBe(0);
    expect(nextRovingIndex(-1, 5, "ArrowLeft")).toBe(4);
    expect(nextRovingIndex(-1, 5, "End")).toBe(4);
  });

  it("clamps an out-of-range current index instead of crashing", () => {
    expect(nextRovingIndex(99, 3, "ArrowRight")).toBe(0);
    expect(nextRovingIndex(Number.NaN, 3, "ArrowLeft")).toBe(2);
  });

  it("returns null for empty lists", () => {
    expect(nextRovingIndex(0, 0, "ArrowRight")).toBeNull();
  });

  it("handles a single-item list without dividing by zero", () => {
    expect(nextRovingIndex(0, 1, "ArrowRight")).toBe(0);
    expect(nextRovingIndex(0, 1, "ArrowLeft")).toBe(0);
  });
});

describe("vertical lists", () => {
  it("moves with ArrowUp/ArrowDown and wraps", () => {
    expect(nextRovingIndex(0, 3, "ArrowDown", "vertical")).toBe(1);
    expect(nextRovingIndex(2, 3, "ArrowDown", "vertical")).toBe(0);
    expect(nextRovingIndex(0, 3, "ArrowUp", "vertical")).toBe(2);
  });

  it("ignores the other orientation's arrows in both directions", () => {
    // A vertical list must not swallow ArrowLeft (the caller may want it for
    // something else), and a horizontal one must not swallow ArrowUp.
    expect(nextRovingIndex(0, 3, "ArrowLeft", "vertical")).toBeNull();
    expect(nextRovingIndex(0, 3, "ArrowRight", "vertical")).toBeNull();
    expect(nextRovingIndex(0, 3, "ArrowUp", "horizontal")).toBeNull();
    expect(nextRovingIndex(0, 3, "ArrowDown", "horizontal")).toBeNull();
  });

  it("honors travel direction when entering from outside", () => {
    expect(nextRovingIndex(-1, 4, "ArrowDown", "vertical")).toBe(0);
    expect(nextRovingIndex(-1, 4, "ArrowUp", "vertical")).toBe(3);
  });

  it("answers Home and End the same way in both orientations", () => {
    for (const orientation of ["horizontal", "vertical"] as const) {
      expect(nextRovingIndex(-1, 4, "Home", orientation)).toBe(0);
      expect(nextRovingIndex(2, 4, "Home", orientation)).toBe(0);
      expect(nextRovingIndex(-1, 4, "End", orientation)).toBe(3);
      expect(nextRovingIndex(2, 4, "End", orientation)).toBe(3);
    }
  });

  it("defaults to horizontal, so existing callers are unchanged", () => {
    expect(nextRovingIndex(0, 3, "ArrowRight")).toBe(1);
  });
});
