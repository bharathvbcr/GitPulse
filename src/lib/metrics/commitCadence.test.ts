import { describe, expect, it } from "vitest";
import {
  activeDayCount,
  bucketCommitsByDay,
  MAX_BUCKETS,
  sparklineHeights,
} from "./commitCadence";

/** Epoch seconds for a local date-time, so tests read as calendar dates. */
function at(year: number, month: number, day: number, hour = 12, minute = 0): number {
  return Math.floor(new Date(year, month - 1, day, hour, minute, 0, 0).getTime() / 1000);
}

const NOW = new Date(2026, 2, 15, 18, 30, 0, 0).getTime(); // 15 March 2026, local

describe("commit cadence bucketing", () => {
  it("returns one bucket per day, oldest first", () => {
    const summary = bucketCommitsByDay([], 7, NOW);
    expect(summary.buckets).toHaveLength(7);
    const days = summary.buckets.map((b) => b.day);
    expect(days).toEqual([...days].sort());
    expect(days.at(-1)).toBe("2026-03-15");
  });

  it("handles an empty history without inventing activity", () => {
    const summary = bucketCommitsByDay([], 7, NOW);
    expect(summary.total).toBe(0);
    expect(summary.peak).toBe(0);
    expect(summary.mean).toBe(0);
    expect(summary.partial).toBe(true);
    // No commits must not divide by zero.
    expect(sparklineHeights(summary)).toEqual([0, 0, 0, 0, 0, 0, 0]);
    expect(activeDayCount(summary)).toBe(0);
  });

  it("handles a single commit", () => {
    const summary = bucketCommitsByDay([{ timestamp: at(2026, 3, 13) }], 7, NOW);
    expect(summary.total).toBe(1);
    expect(summary.peak).toBe(1);
    expect(activeDayCount(summary)).toBe(1);
    expect(summary.buckets.find((b) => b.day === "2026-03-13")?.count).toBe(1);
    expect(sparklineHeights(summary).filter((h) => h === 1)).toHaveLength(1);
  });

  it("puts every commit in one bucket when they all land on one day", () => {
    const commits = [0, 3, 7, 11, 23].map((hour) => ({ timestamp: at(2026, 3, 14, hour) }));
    const summary = bucketCommitsByDay(commits, 7, NOW);
    expect(summary.total).toBe(5);
    expect(summary.peak).toBe(5);
    expect(activeDayCount(summary)).toBe(1);
    expect(summary.buckets.find((b) => b.day === "2026-03-14")?.count).toBe(5);
  });

  it("keeps calendar-day boundaries across a DST transition", () => {
    // US DST began 8 March 2026, so 8 March is a 23-hour local day. Fixed
    // 86400s windows would drift every later boundary by an hour.
    const now = new Date(2026, 2, 10, 12, 0, 0, 0).getTime();
    const commits = [
      { timestamp: at(2026, 3, 7, 23, 30) }, // before the transition
      { timestamp: at(2026, 3, 8, 3, 30) }, // after the transition, same window
      { timestamp: at(2026, 3, 9, 0, 30) }, // just after local midnight
    ];
    const summary = bucketCommitsByDay(commits, 5, now);
    const byDay = Object.fromEntries(summary.buckets.map((b) => [b.day, b.count]));
    expect(byDay["2026-03-07"]).toBe(1);
    expect(byDay["2026-03-08"]).toBe(1);
    expect(byDay["2026-03-09"]).toBe(1);
    expect(summary.total).toBe(3);
  });

  it("excludes commits outside the window rather than clamping them", () => {
    const commits = [
      { timestamp: at(2025, 1, 1) }, // long before
      { timestamp: at(2026, 3, 14) }, // inside
      { timestamp: at(2027, 1, 1) }, // dated in the future
    ];
    const summary = bucketCommitsByDay(commits, 7, NOW);
    // Clamping the outliers into edge buckets would invent activity.
    expect(summary.total).toBe(1);
    expect(summary.buckets[0].count).toBe(0);
    expect(summary.buckets.at(-1)?.count).toBe(0);
  });

  it("reports a history shorter than the window as partial", () => {
    const shortHistory = bucketCommitsByDay([{ timestamp: at(2026, 3, 14) }], 30, NOW);
    expect(shortHistory.partial).toBe(true);

    const longHistory = bucketCommitsByDay(
      [{ timestamp: at(2026, 1, 1) }, { timestamp: at(2026, 3, 14) }],
      30,
      NOW,
    );
    expect(longHistory.partial).toBe(false);
  });

  it("ignores unusable timestamps instead of producing NaN buckets", () => {
    const summary = bucketCommitsByDay(
      [
        { timestamp: Number.NaN },
        { timestamp: Number.POSITIVE_INFINITY },
        { timestamp: 0 },
        { timestamp: -1 },
        { timestamp: at(2026, 3, 14) },
      ],
      7,
      NOW,
    );
    expect(summary.total).toBe(1);
    expect(summary.buckets.every((b) => Number.isFinite(b.count))).toBe(true);
    expect(summary.buckets.every((b) => /^\d{4}-\d{2}-\d{2}$/.test(b.day))).toBe(true);
  });

  it("clamps the window rather than allocating an unbounded axis", () => {
    expect(bucketCommitsByDay([], 0, NOW).buckets).toHaveLength(1);
    expect(bucketCommitsByDay([], -5, NOW).buckets).toHaveLength(1);
    expect(bucketCommitsByDay([], 10_000, NOW).buckets).toHaveLength(MAX_BUCKETS);
    expect(bucketCommitsByDay([], Number.NaN, NOW).buckets).toHaveLength(1);
  });

  it("returns nothing usable when 'now' is not a real instant", () => {
    const summary = bucketCommitsByDay([{ timestamp: at(2026, 3, 14) }], 7, Number.NaN);
    expect(summary.buckets).toHaveLength(0);
    expect(summary.total).toBe(0);
  });

  it("scales heights against the peak, not the total", () => {
    const commits = [
      ...Array.from({ length: 4 }, () => ({ timestamp: at(2026, 3, 14) })),
      { timestamp: at(2026, 3, 13) },
    ];
    const heights = sparklineHeights(bucketCommitsByDay(commits, 7, NOW));
    expect(Math.max(...heights)).toBe(1);
    expect(heights).toContain(0.25);
  });
});
