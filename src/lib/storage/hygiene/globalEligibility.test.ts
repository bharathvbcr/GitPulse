import { describe, expect, it } from "vitest";
import { cleanupAllowed, coverageNotice } from "./globalEligibility";
import type { CleanerCandidate, CleanerInventory } from "./globalTypes";

const candidate: CleanerCandidate = {
  repo_path: "/w/repo",
  path: "target",
  provider: "Rust / Cargo",
  bytes: 1,
};

function inventory(over: Partial<CleanerInventory> = {}): CleanerInventory {
  return {
    candidates: [candidate],
    repositories: 1,
    visited_entries: 10,
    partial: false,
    issues: [],
    ...over,
  };
}

describe("cleanupAllowed", () => {
  it("allows a complete inventory that found something", () => {
    expect(cleanupAllowed(inventory())).toBe(true);
  });

  // The whole point of the per-artifact scan budget: a tree the scan read to
  // the end is cleanable even though some other part of the workspace was not
  // read. Gating on `partial` greyed this out for the entire workspace while
  // the scheduled sweep cleaned it anyway.
  it("allows a partial inventory that still found candidates", () => {
    expect(cleanupAllowed(inventory({ partial: true }))).toBe(true);
  });

  it("refuses when nothing is eligible, complete scan or not", () => {
    expect(cleanupAllowed(inventory({ candidates: [] }))).toBe(false);
    expect(cleanupAllowed(inventory({ candidates: [], partial: true }))).toBe(false);
  });

  it("refuses before an inventory has been taken", () => {
    expect(cleanupAllowed(null)).toBe(false);
  });
});

describe("coverageNotice", () => {
  it("says nothing when the scan covered everything", () => {
    expect(coverageNotice(inventory())).toBeNull();
    expect(coverageNotice(null)).toBeNull();
  });

  // The copy is the other half of the fix. It used to say cleanup was blocked
  // until a complete scan succeeded and tell the user to narrow their roots,
  // which was false and pointed them at the wrong remedy.
  it("reports incomplete coverage without claiming cleanup is blocked", () => {
    const notice = coverageNotice(inventory({ partial: true }));
    expect(notice).toContain("Incomplete inventory");
    expect(notice).toContain("stays eligible");
    expect(notice).not.toContain("blocked");
    expect(notice).not.toContain("narrow");
  });

  it("explains the one case where incomplete coverage does leave nothing to do", () => {
    const notice = coverageNotice(inventory({ candidates: [], partial: true }));
    expect(notice).toContain("nothing eligible was found");
  });
});
