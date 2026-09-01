import { describe, expect, it } from "vitest";
import {
  formatAge,
  hoursToFirstReview,
  isAwaitingFirstReview,
  openHours,
  parseTimestamp,
  summarizeVelocity,
  type PullRequestTiming,
} from "./prVelocity";

const NOW = Date.parse("2026-06-01T12:00:00Z");

function hoursAgo(hours: number): string {
  return new Date(NOW - hours * 3_600_000).toISOString();
}

function pr(overrides: Partial<PullRequestTiming> = {}): PullRequestTiming {
  return { created_at: hoursAgo(24), first_review_at: "", is_draft: false, ...overrides };
}

describe("timestamp parsing", () => {
  it("keeps 'absent' distinct from 'the epoch'", () => {
    // Returning 0 would render an unknown creation date as a 56-year-old PR.
    expect(parseTimestamp("")).toBeNull();
    expect(parseTimestamp("   ")).toBeNull();
    expect(parseTimestamp(null)).toBeNull();
    expect(parseTimestamp(undefined)).toBeNull();
    expect(parseTimestamp("not a date")).toBeNull();
    expect(parseTimestamp("1970-01-01T00:00:00Z")).toBe(0);
  });

  it("parses the RFC 3339 form gh emits", () => {
    expect(parseTimestamp("2026-06-01T12:00:00Z")).toBe(NOW);
  });
});

describe("open age", () => {
  it("measures hours since creation", () => {
    expect(openHours(pr({ created_at: hoursAgo(5) }), NOW)).toBeCloseTo(5);
    expect(openHours(pr({ created_at: hoursAgo(0) }), NOW)).toBeCloseTo(0);
  });

  it("clamps a creation timestamp in the future rather than going negative", () => {
    expect(openHours(pr({ created_at: hoursAgo(-10) }), NOW)).toBe(0);
  });

  it("returns null when it cannot know", () => {
    expect(openHours(pr({ created_at: "" }), NOW)).toBeNull();
    expect(openHours(pr(), Number.NaN)).toBeNull();
  });
});

describe("time to first review", () => {
  it("measures from creation to the earliest review", () => {
    const reviewed = pr({ created_at: hoursAgo(48), first_review_at: hoursAgo(40) });
    expect(hoursToFirstReview(reviewed)).toBeCloseTo(8);
  });

  it("is null for an unreviewed pull request, not zero", () => {
    // Zero would read as "reviewed instantly", the opposite of the truth.
    expect(hoursToFirstReview(pr({ first_review_at: "" }))).toBeNull();
    expect(isAwaitingFirstReview(pr({ first_review_at: "" }))).toBe(true);
  });

  it("does not count a draft as waiting on anyone", () => {
    expect(isAwaitingFirstReview(pr({ is_draft: true, first_review_at: "" }))).toBe(false);
  });

  it("clamps a review recorded before creation", () => {
    const skewed = pr({ created_at: hoursAgo(10), first_review_at: hoursAgo(20) });
    expect(hoursToFirstReview(skewed)).toBe(0);
  });
});

describe("age formatting", () => {
  it("degrades from hours to days to weeks", () => {
    expect(formatAge(0.4)).toBe("<1h");
    expect(formatAge(5)).toBe("5h");
    expect(formatAge(30)).toBe("1d");
    expect(formatAge(24 * 20)).toBe("2w");
    expect(formatAge(null)).toBe("—");
  });
});

describe("velocity summary", () => {
  it("returns an empty summary rather than zeros for no pull requests", () => {
    const summary = summarizeVelocity([], NOW);
    expect(summary.considered).toBe(0);
    // Null means "nothing to measure"; 0 would claim PRs are merged instantly.
    expect(summary.medianOpenHours).toBeNull();
    expect(summary.medianFirstReviewHours).toBeNull();
    expect(summary.oldestOpenHours).toBeNull();
    expect(summary.awaitingReview).toBe(0);
  });

  it("excludes drafts from every figure", () => {
    const summary = summarizeVelocity(
      [pr({ created_at: hoursAgo(100), is_draft: true }), pr({ created_at: hoursAgo(10) })],
      NOW,
    );
    expect(summary.considered).toBe(1);
    expect(summary.medianOpenHours).toBeCloseTo(10);
    expect(summary.oldestOpenHours).toBeCloseTo(10);
  });

  it("uses the median so one ancient pull request does not swamp the number", () => {
    const summary = summarizeVelocity(
      [
        pr({ created_at: hoursAgo(1) }),
        pr({ created_at: hoursAgo(2) }),
        pr({ created_at: hoursAgo(24 * 365) }),
      ],
      NOW,
    );
    expect(summary.medianOpenHours).toBeCloseTo(2);
    // The outlier is still visible, just not as the headline.
    expect(summary.oldestOpenHours).toBeCloseTo(24 * 365);
  });

  it("averages the two middle values for an even count", () => {
    const summary = summarizeVelocity(
      [
        pr({ created_at: hoursAgo(2) }),
        pr({ created_at: hoursAgo(4) }),
        pr({ created_at: hoursAgo(6) }),
        pr({ created_at: hoursAgo(8) }),
      ],
      NOW,
    );
    expect(summary.medianOpenHours).toBeCloseTo(5);
  });

  it("counts the unreviewed and measures only those that were reviewed", () => {
    const summary = summarizeVelocity(
      [
        pr({ created_at: hoursAgo(48), first_review_at: hoursAgo(46) }),
        pr({ created_at: hoursAgo(48), first_review_at: "" }),
        pr({ created_at: hoursAgo(10), first_review_at: "" }),
      ],
      NOW,
    );
    expect(summary.awaitingReview).toBe(2);
    // The unreviewed must not be folded in as a 0-hour wait.
    expect(summary.medianFirstReviewHours).toBeCloseTo(2);
  });

  it("ignores pull requests with no usable creation timestamp", () => {
    const summary = summarizeVelocity([pr({ created_at: "" }), pr({ created_at: hoursAgo(6) })], NOW);
    expect(summary.considered).toBe(2);
    expect(summary.medianOpenHours).toBeCloseTo(6);
  });
});
