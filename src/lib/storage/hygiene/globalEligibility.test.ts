import { describe, expect, it } from "vitest";
import { cleanupAllowed, coverageNotice, itemStatusLabel, runRowsSummary } from "./globalEligibility";
import type { CleanerCandidate, CleanerInventory, CleanerRun } from "./globalTypes";

const candidate: CleanerCandidate = {
  repo_path: "/w/repo",
  path: "target",
  provider: "Rust / Cargo",
  scope: "repository",
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

describe("run rows", () => {
  const run = (over: Partial<CleanerRun> = {}): CleanerRun => ({
    id: "r", trigger: "scheduled", started_at: 0, finished_at: 1, status: "completed_with_skips",
    repositories: 3, partial: false, issues: [], items: [], ...over,
  });
  it("never shows a check the host shed as a refusal", () => {
    expect(itemStatusLabel("unexamined")).toBe("could not examine");
    expect(itemStatusLabel("completed_with_skips")).toBe("completed with skips");
  });
  it("counts rows a run kept and the ones it could not list", () => {
    expect(runRowsSummary(run())).toBe("0 targets reported");
    expect(runRowsSummary(run({ unlisted: 70, unexamined: 14 }))).toBe(
      "0 targets reported · 70 more not listed · 14 could not be examined under load",
    );
  });
});
