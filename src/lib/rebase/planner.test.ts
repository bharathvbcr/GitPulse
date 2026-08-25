import { describe, expect, it } from "vitest";
import { seedRebasePlan, shouldReseed, shouldSeed } from "./planner";
import type { VisualCommitRow } from "../canvas/GraphRenderer";

function row(id: string): VisualCommitRow {
  return {
    id,
    parent_ids: [],
    summary: `summary ${id}`,
    author_name: "a",
    author_email: "a@x",
    timestamp: 1,
    lane: 0,
    color_index: 0,
    active_lanes: [0],
    active_lane_colors: [0],
    connections: [],
    is_merge: false,
    is_root: false,
  };
}

describe("seedRebasePlan", () => {
  it("orders oldest-first and caps the window", () => {
    // Store history is newest-first; the plan must run oldest-first.
    const commits = ["c3", "c2", "c1"].map(row);
    const plan = seedRebasePlan(commits);
    expect(plan.map((p) => p.id)).toEqual(["c1", "c2", "c3"]);
    expect(plan.every((p) => p.action === "Pick")).toBe(true);
  });

  it("caps at the window size keeping the newest commits", () => {
    // c0 is newest … c19 is oldest.
    const commits = Array.from({ length: 20 }, (_, i) => row(`c${i}`));
    const plan = seedRebasePlan(commits, 12);
    expect(plan).toHaveLength(12);
    expect(plan[0]?.id).toBe("c11");
    expect(plan[11]?.id).toBe("c0");
  });

  it("empty history seeds an empty plan", () => {
    expect(seedRebasePlan([])).toEqual([]);
  });
});

describe("shouldSeed", () => {
  it("seeds only on the closed-to-open transition", () => {
    expect(shouldSeed(true, false)).toBe(true);
    expect(shouldSeed(true, true)).toBe(false);
    expect(shouldSeed(false, true)).toBe(false);
    expect(shouldSeed(false, false)).toBe(false);
  });
});

describe("shouldReseed", () => {
  it("follows history changes only while the plan is pristine", () => {
    expect(
      shouldReseed({
        isOpen: true,
        wasOpen: true,
        dirty: false,
        currentSignature: "a,b",
        seededSignature: "a",
      })
    ).toBe(true);
    expect(
      shouldReseed({
        isOpen: true,
        wasOpen: true,
        dirty: true,
        currentSignature: "a,b",
        seededSignature: "a",
      })
    ).toBe(false);
  });

  it("an identical signature never reseeds, even pristine", () => {
    expect(
      shouldReseed({
        isOpen: true,
        wasOpen: true,
        dirty: false,
        currentSignature: "a,b",
        seededSignature: "a,b",
      })
    ).toBe(false);
  });

  it("a closed dialog never reseeds", () => {
    expect(
      shouldReseed({
        isOpen: false,
        wasOpen: true,
        dirty: false,
        currentSignature: "z",
        seededSignature: "a",
      })
    ).toBe(false);
  });
});
