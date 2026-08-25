import { describe, it, expect } from "vitest";
import { INITIAL_GRAPH_PAINT, stepGraphPaint } from "../graphPaint";

describe("stepGraphPaint", () => {
  it("snaps hover and selection when reduced motion is on", () => {
    const { next, animating } = stepGraphPaint(INITIAL_GRAPH_PAINT, {
      hoveredCommitId: "abc",
      selectionReset: true,
      deltaMs: 16,
      reducedMotion: true,
    });
    expect(next.hoverStrength).toBe(1);
    expect(next.selectionStrength).toBe(1);
    expect(next.displayHoverId).toBe("abc");
    expect(animating).toBe(false);
  });

  it("clears the hover target immediately under reduced motion", () => {
    const { next } = stepGraphPaint(
      { hoverStrength: 1, selectionStrength: 1, displayHoverId: "abc" },
      { hoveredCommitId: null, selectionReset: false, deltaMs: 16, reducedMotion: true },
    );
    expect(next.hoverStrength).toBe(0);
    expect(next.displayHoverId).toBeNull();
  });

  it("approaches hover without reaching it in one 16ms frame", () => {
    const { next, animating } = stepGraphPaint(INITIAL_GRAPH_PAINT, {
      hoveredCommitId: "abc",
      selectionReset: false,
      deltaMs: 16,
      reducedMotion: false,
    });
    expect(next.hoverStrength).toBeGreaterThan(0);
    expect(next.hoverStrength).toBeLessThan(1);
    expect(next.displayHoverId).toBe("abc");
    expect(animating).toBe(true);
  });

  it("keeps the last hovered id while the ring fades out", () => {
    const { next } = stepGraphPaint(
      { hoverStrength: 1, selectionStrength: 1, displayHoverId: "abc" },
      { hoveredCommitId: null, selectionReset: false, deltaMs: 16, reducedMotion: false },
    );
    expect(next.hoverStrength).toBeLessThan(1);
    expect(next.hoverStrength).toBeGreaterThan(0);
    expect(next.displayHoverId).toBe("abc");
  });

  it("resets selection strength then damps toward 1", () => {
    const { next, animating } = stepGraphPaint(
      { hoverStrength: 0, selectionStrength: 1, displayHoverId: null },
      { hoveredCommitId: null, selectionReset: true, deltaMs: 16, reducedMotion: false },
    );
    expect(next.selectionStrength).toBeGreaterThan(0);
    expect(next.selectionStrength).toBeLessThan(1);
    expect(animating).toBe(true);
  });
});
