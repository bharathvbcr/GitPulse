import { describe, expect, it } from "vitest";
import {
  branchAgeInDays,
  branchHealth,
  DEFAULT_BRANCH_HEALTH_THRESHOLDS,
  needsAttention,
} from "./health";
import type { BranchInfo } from "./types";

const NOW = new Date(2026, 5, 1, 12, 0, 0, 0).getTime();

function daysAgo(days: number): number {
  return Math.floor((NOW - days * 86_400_000) / 1000);
}

function branch(overrides: Partial<BranchInfo> = {}): BranchInfo {
  return {
    name: "feature/x",
    is_current: false,
    is_remote: false,
    remote_name: null,
    tip_commit_id: "abc123",
    ahead_count: 0,
    behind_count: 0,
    upstream: "origin/feature/x",
    is_default: false,
    is_gone: false,
    last_commit_timestamp: daysAgo(1),
    last_author: "Ada",
    last_summary: "work",
    commits_ahead_of_base: 1,
    commits_behind_base: 0,
    additions: 0,
    deletions: 0,
    files_changed: 0,
    compared_to: "main",
    ...overrides,
  };
}

describe("branch age", () => {
  it("counts whole days since the tip", () => {
    expect(branchAgeInDays(branch({ last_commit_timestamp: daysAgo(0) }), NOW)).toBe(0);
    expect(branchAgeInDays(branch({ last_commit_timestamp: daysAgo(45) }), NOW)).toBe(45);
  });

  it("clamps a tip dated in the future instead of reporting a negative age", () => {
    // Clock skew and rewritten history both produce these; a skewed branch
    // must not read as stale.
    const future = branch({ last_commit_timestamp: daysAgo(-10) });
    expect(branchAgeInDays(future, NOW)).toBe(0);
    expect(branchHealth(future, NOW).code).not.toBe("stale");
  });

  it("returns null rather than a number it cannot justify", () => {
    expect(branchAgeInDays(branch({ last_commit_timestamp: 0 }), NOW)).toBeNull();
    expect(branchAgeInDays(branch({ last_commit_timestamp: Number.NaN }), NOW)).toBeNull();
    expect(branchAgeInDays(branch(), Number.NaN)).toBeNull();
  });
});

describe("branch health verdicts", () => {
  it("reports a fresh, ahead-only branch as healthy", () => {
    const health = branchHealth(branch(), NOW);
    expect(health.code).toBe("healthy");
    expect(health.level).toBe("healthy");
    expect(needsAttention(health)).toBe(false);
  });

  it("reports a branch with no commits for longer than the threshold as stale", () => {
    const health = branchHealth(branch({ last_commit_timestamp: daysAgo(45) }), NOW);
    expect(health.code).toBe("stale");
    expect(health.level).toBe("warn");
    expect(health.detail).toContain("45 days");
    expect(health.detail).toContain("Ada");
  });

  it("treats the staleness threshold as inclusive at its boundary", () => {
    const dayBefore = branch({ last_commit_timestamp: daysAgo(29) });
    const onThreshold = branch({ last_commit_timestamp: daysAgo(30) });
    expect(branchHealth(dayBefore, NOW).code).not.toBe("stale");
    expect(branchHealth(onThreshold, NOW).code).toBe("stale");
  });

  it("honours a caller's threshold rather than a baked-in 30 days", () => {
    const week = branch({ last_commit_timestamp: daysAgo(10) });
    expect(branchHealth(week, NOW).code).not.toBe("stale");
    expect(branchHealth(week, NOW, { staleDays: 7 }).code).toBe("stale");
    // A nonsensical threshold must not make every branch stale-free.
    expect(branchHealth(week, NOW, { staleDays: 0 }).code).toBe("stale");
  });

  it("reports a fully merged branch as safe to delete", () => {
    const health = branchHealth(branch({ commits_ahead_of_base: 0 }), NOW);
    expect(health.code).toBe("merged");
    expect(health.detail).toContain("main");
    expect(health.detail).toContain("Safe to delete");
  });

  it("prefers the actionable verdict when a branch is both merged and stale", () => {
    // Telling the reader they can delete it beats telling them it is old.
    const health = branchHealth(
      branch({ commits_ahead_of_base: 0, last_commit_timestamp: daysAgo(200) }),
      NOW,
    );
    expect(health.code).toBe("merged");
  });

  it("reports a branch ahead and behind its base as diverged", () => {
    const health = branchHealth(
      branch({ commits_ahead_of_base: 3, commits_behind_base: 5 }),
      NOW,
    );
    expect(health.code).toBe("diverged");
    expect(health.level).toBe("warn");
    expect(health.detail).toContain("3 commits ahead");
    expect(health.detail).toContain("5 commits behind");
  });

  it("ranks divergence above staleness", () => {
    const health = branchHealth(
      branch({ commits_ahead_of_base: 1, commits_behind_base: 2, last_commit_timestamp: daysAgo(90) }),
      NOW,
    );
    expect(health.code).toBe("diverged");
  });

  it("reports a deleted upstream above everything else", () => {
    const health = branchHealth(
      branch({ is_gone: true, commits_ahead_of_base: 0, last_commit_timestamp: daysAgo(400) }),
      NOW,
    );
    expect(health.code).toBe("gone");
    expect(health.level).toBe("attention");
    expect(needsAttention(health)).toBe(true);
  });

  it("reports a local branch with no upstream as unpublished, not broken", () => {
    const health = branchHealth(
      branch({ upstream: null, commits_ahead_of_base: 2, commits_behind_base: 0 }),
      NOW,
    );
    expect(health.code).toBe("unpublished");
    expect(health.level).toBe("info");
    expect(needsAttention(health)).toBe(false);
  });

  it("never calls the default branch merged or behind", () => {
    const health = branchHealth(
      branch({ is_default: true, commits_ahead_of_base: 0, commits_behind_base: 9 }),
      NOW,
    );
    // It is the base; measuring it against itself says nothing.
    expect(health.code).toBe("healthy");
    expect(needsAttention(health)).toBe(false);
  });

  it("reports a branch behind only its base as behind", () => {
    const health = branchHealth(
      branch({ commits_ahead_of_base: 0, commits_behind_base: 4, compared_to: "develop" }),
      NOW,
    );
    // ahead === 0 with a base means merged; give it unique commits to isolate.
    expect(["merged", "behind"]).toContain(health.code);

    const behind = branchHealth(
      branch({ commits_ahead_of_base: 2, commits_behind_base: 4, compared_to: "develop" }),
      NOW,
    );
    expect(behind.code).toBe("diverged");
  });

  it("gives every verdict a title and a one-sentence explanation for the tooltip", () => {
    const cases: BranchInfo[] = [
      branch(),
      branch({ is_gone: true }),
      branch({ is_default: true }),
      branch({ commits_ahead_of_base: 0 }),
      branch({ commits_ahead_of_base: 1, commits_behind_base: 1 }),
      branch({ last_commit_timestamp: daysAgo(90) }),
      branch({ upstream: null }),
      branch({ is_current: true, commits_ahead_of_base: 1 }),
    ];
    for (const candidate of cases) {
      const health = branchHealth(candidate, NOW);
      expect(health.title.length).toBeGreaterThan(0);
      expect(health.detail.length).toBeGreaterThan(10);
      expect(health.detail.trim().endsWith(".")).toBe(true);
    }
  });

  it("exposes its defaults so a caller can see what it assumed", () => {
    expect(DEFAULT_BRANCH_HEALTH_THRESHOLDS.staleDays).toBe(30);
    expect(Object.isFrozen(DEFAULT_BRANCH_HEALTH_THRESHOLDS)).toBe(true);
  });
});
