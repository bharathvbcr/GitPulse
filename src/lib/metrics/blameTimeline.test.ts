import { describe, expect, it } from "vitest";
import type { BlameLine } from "../files/types";
import {
  AGE_BANDS,
  MAX_PERIODS,
  ageBandForDays,
  bandColor,
  blameBucketKey,
  blameRowTint,
  buildAgeTicks,
  buildBlameTimeline,
  columnHeight,
  describePeriod,
  formatShare,
  isUncommittedLine,
  sameSelection,
  selectionLabel,
  selectionMatches,
  toneColor,
  type BlameSelection,
} from "./blameTimeline";

/** Epoch SECONDS for a local date-time, so tests read as calendar dates. */
function at(year: number, month: number, day: number, hour = 12): number {
  return Math.floor(new Date(year, month - 1, day, hour, 0, 0, 0).getTime() / 1000);
}

const NOW = new Date(2026, 8, 19, 15, 0, 0, 0).getTime(); // 19 September 2026, local

let nextOid = 0;

function line(overrides: Partial<BlameLine> = {}): BlameLine {
  nextOid += 1;
  return {
    line_no: nextOid,
    commit_id: `${nextOid}`.padStart(40, "a"),
    author_name: "Ada",
    author_email: "ada@example.com",
    timestamp: at(2026, 9, 1),
    content: "code",
    ...overrides,
  };
}

/** `count` lines sharing one commit and date. */
function block(count: number, overrides: Partial<BlameLine> = {}): BlameLine[] {
  const commit = overrides.commit_id ?? `${(nextOid += 1)}`.padStart(40, "b");
  return Array.from({ length: count }, () => line({ ...overrides, commit_id: commit }));
}

const ZERO = "0".repeat(40);
const ZERO_SHA256 = "0".repeat(64);

describe("buildBlameTimeline denominators", () => {
  it("returns an empty, NaN-free timeline for an empty file", () => {
    const timeline = buildBlameTimeline([], NOW);
    expect(timeline.periods).toEqual([]);
    expect(timeline.extras).toEqual([]);
    expect(timeline.totalLines).toBe(0);
    expect(timeline.peakPercent).toBe(0);
    expect(timeline.medianAgeDays).toBeNull();
    expect(timeline.newestTimestamp).toBeNull();
    expect(timeline.oldestTimestamp).toBeNull();
  });

  it("survives a non-finite now instead of building an axis from NaN", () => {
    const timeline = buildBlameTimeline([line()], Number.NaN);
    expect(timeline.periods).toEqual([]);
    expect(timeline.totalLines).toBe(0);
    expect(timeline.nowMs).toBe(0);
  });

  it("spends the whole file: periods plus extras account for 100%", () => {
    const lines = [
      ...block(40, { timestamp: at(2026, 9, 10) }),
      ...block(30, { timestamp: at(2025, 4, 2) }),
      ...block(20, { timestamp: at(2023, 1, 5) }),
      ...block(6, { commit_id: ZERO, timestamp: at(2026, 9, 19) }),
      ...block(3, { timestamp: 0 }),
      ...block(1, { timestamp: at(2030, 1, 1) }),
    ];
    const timeline = buildBlameTimeline(lines, NOW);

    expect(timeline.totalLines).toBe(100);
    const drawn =
      timeline.periods.reduce((sum, period) => sum + period.percent, 0) +
      timeline.extras.reduce((sum, extra) => sum + extra.percent, 0);
    expect(drawn).toBeCloseTo(100, 9);

    const counted =
      timeline.periods.reduce((sum, period) => sum + period.lines, 0) +
      timeline.extras.reduce((sum, extra) => sum + extra.lines, 0);
    expect(counted).toBe(timeline.totalLines);
  });

  it("names the lines a time axis cannot hold rather than dropping them", () => {
    const timeline = buildBlameTimeline(
      [
        ...block(5, { timestamp: at(2026, 6, 1) }),
        ...block(4, { commit_id: ZERO, timestamp: at(2026, 9, 19) }),
        ...block(3, { timestamp: Number.NaN }),
        ...block(2, { timestamp: at(2031, 5, 5) }),
      ],
      NOW,
    );
    const byKey = Object.fromEntries(timeline.extras.map((extra) => [extra.key, extra.lines]));
    expect(byKey).toEqual({ uncommitted: 4, future: 2, undated: 3 });
    // Order is the one they are drawn in: newest-flavoured first.
    expect(timeline.extras.map((extra) => extra.key)).toEqual([
      "uncommitted",
      "future",
      "undated",
    ]);
    expect(timeline.datedLines).toBe(5);
  });

  it("omits an empty extra so a clean file shows no chips", () => {
    const timeline = buildBlameTimeline(block(3, { timestamp: at(2026, 9, 1) }), NOW);
    expect(timeline.extras).toEqual([]);
  });

  it("treats a dated worktree line as uncommitted, not as fresh history", () => {
    // git dates a not-yet-committed line with the current time. The OID, not
    // the timestamp, is what says the line has no commit.
    for (const zero of [ZERO, ZERO_SHA256]) {
      const timeline = buildBlameTimeline(
        [line({ commit_id: zero, timestamp: at(2026, 9, 19) })],
        NOW,
      );
      expect(timeline.extras.map((extra) => extra.key)).toEqual(["uncommitted"]);
      expect(timeline.datedLines).toBe(0);
      // And it is not counted as a commit or an author of record.
      expect(timeline.commits).toBe(0);
      expect(timeline.authors).toBe(0);
    }
  });

  it("rejects a zero-prefixed real commit as uncommitted only when it is all zeros", () => {
    expect(isUncommittedLine(line({ commit_id: ZERO }))).toBe(true);
    expect(isUncommittedLine(line({ commit_id: `0000000${"c".repeat(33)}` }))).toBe(false);
  });

  it("counts a negative or zero timestamp as undated, not as 1970", () => {
    const timeline = buildBlameTimeline(
      [line({ timestamp: -1 }), line({ timestamp: 0 }), line({ timestamp: at(2026, 9, 1) })],
      NOW,
    );
    expect(timeline.extras.find((extra) => extra.key === "undated")?.lines).toBe(2);
    expect(timeline.oldestTimestamp).toBe(at(2026, 9, 1));
  });
});

describe("the axis", () => {
  it("runs from the oldest line to the period containing now, with gaps drawn", () => {
    const timeline = buildBlameTimeline(
      [
        ...block(10, { timestamp: at(2025, 6, 4) }),
        ...block(10, { timestamp: at(2026, 9, 2) }),
      ],
      NOW,
    );
    expect(timeline.granularity).toBe("month");
    const labels = timeline.periods.map((period) => period.label);
    expect(labels[0]).toBe("Jun 2025");
    expect(labels[labels.length - 1]).toBe("Sep 2026");
    expect(labels).toHaveLength(16);
    // The quiet months are present and empty rather than missing.
    expect(timeline.periods.filter((period) => period.lines === 0)).toHaveLength(14);
  });

  it("is contiguous: each period ends exactly where the next begins", () => {
    const timeline = buildBlameTimeline(
      [...block(4, { timestamp: at(2019, 2, 1) }), ...block(4, { timestamp: at(2026, 9, 1) })],
      NOW,
    );
    for (let i = 1; i < timeline.periods.length; i += 1) {
      expect(timeline.periods[i - 1].end).toBe(timeline.periods[i].start);
      expect(timeline.periods[i].start).toBeGreaterThan(timeline.periods[i - 1].start);
    }
    const last = timeline.periods[timeline.periods.length - 1];
    expect(last.start).toBeLessThanOrEqual(NOW);
    expect(last.end).toBeGreaterThan(NOW);
  });

  it("steps to a coarser granularity instead of drawing more columns", () => {
    const spans: Array<[number, string]> = [
      [at(2026, 9, 17), "day"],
      [at(2026, 8, 1), "week"],
      [at(2025, 9, 1), "month"],
      [at(2019, 1, 1), "quarter"],
      [at(2000, 1, 1), "year"],
    ];
    for (const [oldest, granularity] of spans) {
      const timeline = buildBlameTimeline(
        [...block(2, { timestamp: oldest }), ...block(2, { timestamp: at(2026, 9, 18) })],
        NOW,
      );
      expect(timeline.granularity).toBe(granularity);
      expect(timeline.periods.length).toBeLessThanOrEqual(MAX_PERIODS);
      expect(timeline.periods.length).toBeGreaterThan(0);
    }
  });

  it("folds a history older than the widest axis into the leading column", () => {
    // A timestamp this old is usually a broken clock, not fifty years of
    // history. Either way the axis must stay bounded and say what it did.
    const timeline = buildBlameTimeline(
      [...block(3, { timestamp: at(1971, 6, 1) }), ...block(7, { timestamp: at(2026, 9, 1) })],
      NOW,
    );
    expect(timeline.granularity).toBe("year");
    expect(timeline.periods).toHaveLength(MAX_PERIODS);
    expect(timeline.periods[0].foldedOlder).toBe(true);
    expect(timeline.periods[0].lines).toBe(3);
    expect(describePeriod(timeline.periods[0])).toContain("or earlier");
    // Nothing was lost to the fold.
    expect(timeline.periods.reduce((sum, period) => sum + period.lines, 0)).toBe(10);
  });

  it("marks no fold when the axis genuinely reaches the oldest line", () => {
    const timeline = buildBlameTimeline(block(3, { timestamp: at(2026, 9, 1) }), NOW);
    expect(timeline.periods.every((period) => !period.foldedOlder)).toBe(true);
  });

  it("draws no axis at all when nothing on it is dated", () => {
    const timeline = buildBlameTimeline(block(5, { commit_id: ZERO }), NOW);
    expect(timeline.periods).toEqual([]);
    expect(timeline.extras[0]).toMatchObject({ key: "uncommitted", lines: 5, percent: 100 });
  });

  it("keeps calendar period starts across a DST transition", () => {
    // Stepping by 86 400 000 ms instead of by calendar months would shift every
    // boundary after a transition; these starts must stay at local midnight.
    const timeline = buildBlameTimeline(
      [...block(2, { timestamp: at(2025, 1, 15) }), ...block(2, { timestamp: at(2026, 9, 1) })],
      NOW,
    );
    expect(timeline.granularity).toBe("month");
    for (const period of timeline.periods) {
      const start = new Date(period.start);
      expect(start.getDate()).toBe(1);
      expect(start.getHours()).toBe(0);
      expect(start.getMinutes()).toBe(0);
    }
  });

  it("labels quarters and weeks the way a reader names them", () => {
    const quarterly = buildBlameTimeline(
      [...block(2, { timestamp: at(2019, 1, 1) }), ...block(2, { timestamp: at(2026, 9, 1) })],
      NOW,
    );
    expect(quarterly.periods[0].label).toBe("Q1 2019");
    expect(quarterly.periods[quarterly.periods.length - 1].label).toBe("Q3 2026");

    const weekly = buildBlameTimeline(
      [...block(2, { timestamp: at(2026, 8, 1) }), ...block(2, { timestamp: at(2026, 9, 18) })],
      NOW,
    );
    // Weeks start on a Monday.
    for (const period of weekly.periods) {
      expect(new Date(period.start).getDay()).toBe(1);
      expect(period.label.startsWith("Week of ")).toBe(true);
    }
  });
});

describe("the picture and the filter classify identically", () => {
  const lines = [
    ...block(9, { timestamp: at(2026, 9, 10) }),
    ...block(5, { timestamp: at(2026, 5, 4) }),
    ...block(7, { timestamp: at(2024, 11, 30) }),
    ...block(2, { commit_id: ZERO, timestamp: at(2026, 9, 19) }),
    ...block(1, { timestamp: Number.POSITIVE_INFINITY }),
    ...block(3, { timestamp: at(2029, 2, 2) }),
  ];
  const timeline = buildBlameTimeline(lines, NOW);

  it("selects exactly the lines each bucket counted", () => {
    // Derived, not hand-listed: every bucket the builder produced is checked
    // against the filter the UI runs, so a column cannot claim a share it
    // would not select.
    const buckets = [...timeline.periods, ...timeline.extras];
    expect(buckets.length).toBeGreaterThan(3);
    for (const bucket of buckets) {
      const selected = lines.filter((entry) => blameBucketKey(entry, timeline) === bucket.key);
      expect(selected).toHaveLength(bucket.lines);
    }
  });

  it("assigns every line to exactly one bucket that exists", () => {
    const known = new Set([
      ...timeline.periods.map((period) => period.key),
      ...timeline.extras.map((extra) => extra.key),
    ]);
    for (const entry of lines) {
      expect(known.has(blameBucketKey(entry, timeline))).toBe(true);
    }
  });

  it("places a line on a period boundary in the period that starts there", () => {
    const boundary = timeline.periods[1];
    const onEdge = line({ timestamp: Math.floor(boundary.start / 1000) });
    expect(blameBucketKey(onEdge, timeline)).toBe(boundary.key);
    const justBefore = line({ timestamp: Math.floor(boundary.start / 1000) - 1 });
    expect(blameBucketKey(justBefore, timeline)).toBe(timeline.periods[0].key);
  });
});

describe("one selection model behind all three ways in", () => {
  const lines = [
    ...block(8, { timestamp: at(2026, 9, 17) }), // < 7d
    ...block(6, { timestamp: at(2026, 8, 25) }), // < 30d
    ...block(4, { timestamp: at(2024, 2, 2) }), // older
    ...block(2, { commit_id: ZERO }),
  ];
  const timeline = buildBlameTimeline(lines, NOW);

  it("selects exactly what every band, period and chip claimed", () => {
    // Derived, not hand-listed: each of the three ways to filter is checked
    // against the count the surface offering it printed, so a legend swatch
    // cannot advertise a share it would not select.
    const selections: BlameSelection[] = [
      ...timeline.bands.map((share) => ({ kind: "band" as const, id: share.band.id })),
      ...timeline.periods.map((period) => ({ kind: "bucket" as const, key: period.key })),
      ...timeline.extras.map((extra) => ({ kind: "bucket" as const, key: extra.key })),
    ];
    const claimed = [
      ...timeline.bands.map((share) => share.lines),
      ...timeline.periods.map((period) => period.lines),
      ...timeline.extras.map((extra) => extra.lines),
    ];
    expect(selections.length).toBeGreaterThan(6);
    selections.forEach((selection, index) => {
      const selected = lines.filter((entry) => selectionMatches(entry, selection, timeline));
      expect(selected).toHaveLength(claimed[index]);
    });
  });

  it("never lets a band selection take a line with no knowable age", () => {
    const skewed = buildBlameTimeline(
      [...block(3, { timestamp: at(2026, 9, 18) }), ...block(2, { timestamp: at(2030, 1, 1) })],
      NOW,
    );
    const fresh: BlameSelection = { kind: "band", id: "fresh" };
    expect(
      [...block(3, { timestamp: at(2026, 9, 18) })].every((entry) =>
        selectionMatches(entry, fresh, skewed),
      ),
    ).toBe(true);
    expect(selectionMatches(line({ timestamp: at(2030, 1, 1) }), fresh, skewed)).toBe(false);
    expect(selectionMatches(line({ commit_id: ZERO }), fresh, skewed)).toBe(false);
    expect(selectionMatches(line({ timestamp: 0 }), fresh, skewed)).toBe(false);
  });

  it("tells the two selection kinds apart when toggling", () => {
    expect(sameSelection({ kind: "band", id: "fresh" }, { kind: "band", id: "fresh" })).toBe(true);
    expect(sameSelection({ kind: "band", id: "fresh" }, { kind: "band", id: "old" })).toBe(false);
    expect(sameSelection({ kind: "band", id: "fresh" }, { kind: "bucket", key: "fresh" })).toBe(
      false,
    );
    expect(sameSelection({ kind: "bucket", key: "a" }, { kind: "bucket", key: "a" })).toBe(true);
    expect(sameSelection(null, { kind: "bucket", key: "a" })).toBe(false);
    expect(sameSelection(null, null)).toBe(true);
  });

  it("refuses to label a selection this file does not have", () => {
    // The caller shows the whole file on a null label. Filtering to nothing
    // would read as "this file is empty".
    expect(selectionLabel(null, timeline)).toBeNull();
    expect(selectionLabel({ kind: "bucket", key: "month:0" }, timeline)).toBeNull();
    expect(selectionLabel({ kind: "band", id: "settled" }, timeline)).toBeNull();
    expect(selectionLabel({ kind: "band", id: "fresh" }, timeline)).toBe("lines < 7d");
    expect(selectionLabel({ kind: "bucket", key: "uncommitted" }, timeline)).toBe("Uncommitted");
    expect(selectionLabel({ kind: "bucket", key: timeline.periods[0].key }, timeline)).toBe(
      timeline.periods[0].label,
    );
  });
});

describe("the age rail", () => {
  it("maps nothing to nothing, and refuses a degenerate bound", () => {
    expect(buildAgeTicks([], NOW)).toEqual([]);
    expect(buildAgeTicks([line()], NOW, 0)).toEqual([]);
    expect(buildAgeTicks([line()], Number.NaN)).toEqual([]);
  });

  it("covers the list from top to bottom, in order, without overlapping", () => {
    const ticks = buildAgeTicks(block(500, { timestamp: at(2026, 9, 1) }), NOW);
    expect(ticks.length).toBeGreaterThan(0);
    expect(ticks.length).toBeLessThanOrEqual(160);
    expect(ticks[0].topPct).toBe(0);
    for (let i = 1; i < ticks.length; i += 1) {
      expect(ticks[i].topPct).toBeGreaterThan(ticks[i - 1].topPct);
    }
    // The last mark reaches the bottom. It may pass it by up to the minimum
    // tick height — marks are floored so a one-line bucket stays visible on a
    // 400px rail, and the rail clips the overshoot — but it must never stop
    // short, which would read as "the file ends here".
    const last = ticks[ticks.length - 1];
    expect(last.topPct + last.heightPct).toBeGreaterThanOrEqual(99.99);
    expect(last.topPct + last.heightPct).toBeLessThanOrEqual(101);
  });

  it("lets the freshest tone in a bucket win, and says how much of it there is", () => {
    // One fresh line among sixty old ones is exactly what a reader opens the
    // map to find; painting the bucket "old" would hide it. The intensity is
    // what keeps that from reading as a solid block of new code.
    const lines = [
      ...block(59, { timestamp: at(2024, 1, 1) }),
      ...block(1, { timestamp: at(2026, 9, 18) }),
    ];
    const ticks = buildAgeTicks(lines, NOW, 1);
    expect(ticks).toHaveLength(1);
    expect(ticks[0].tone).toBe("fresh");
    expect(ticks[0].weight).toBeCloseTo(1 / 60, 9);
  });

  it("ranks a worktree edit above every commit", () => {
    const ticks = buildAgeTicks(
      [...block(9, { timestamp: at(2026, 9, 18) }), ...block(1, { commit_id: ZERO })],
      NOW,
      1,
    );
    expect(ticks[0].tone).toBe("uncommitted");
  });

  it("falls back to undated only when a bucket has nothing else", () => {
    expect(buildAgeTicks(block(4, { timestamp: 0 }), NOW, 1)[0].tone).toBe("undated");
    // A clock-skewed line has no knowable age either, so it reads the same.
    expect(buildAgeTicks(block(4, { timestamp: at(2030, 1, 1) }), NOW, 1)[0].tone).toBe("undated");
  });

  it("keeps a faint mark visible and a full one at full strength", () => {
    const fresh = AGE_BANDS[0];
    expect(toneColor("fresh", 1)).toBe(`rgba(${fresh.rgb}, ${fresh.fillAlpha.toFixed(3)})`);
    // A single line in a large bucket floors at 0.35 of the band's alpha
    // rather than fading to an invisible mark.
    expect(toneColor("fresh", 0.001)).toBe(
      `rgba(${fresh.rgb}, ${(0.35 * fresh.fillAlpha).toFixed(3)})`,
    );
    expect(toneColor("fresh", Number.NaN)).toBe(
      `rgba(${fresh.rgb}, ${fresh.fillAlpha.toFixed(3)})`,
    );
    // Uncommitted rides the themeable accent, not a fifth fixed hue.
    expect(toneColor("uncommitted", 1)).toBe("rgb(var(--c-accent) / 1.000)");
    expect(toneColor("undated", 1)).toBe("rgba(107, 114, 128, 0.500)");
  });
});

describe("summary readings", () => {
  it("counts distinct commits and authors over committed lines", () => {
    const timeline = buildBlameTimeline(
      [
        ...block(3, { commit_id: "c".repeat(40), author_email: "ada@example.com" }),
        ...block(2, { commit_id: "d".repeat(40), author_email: "ADA@Example.com" }),
        ...block(4, { commit_id: "e".repeat(40), author_email: "grace@example.com" }),
        ...block(2, { commit_id: ZERO }),
      ],
      NOW,
    );
    expect(timeline.commits).toBe(3);
    // One person, two spellings of their address.
    expect(timeline.authors).toBe(2);
  });

  it("identifies an author by name when the history carries no address", () => {
    const timeline = buildBlameTimeline(
      [
        ...block(2, { commit_id: "f".repeat(40), author_email: "", author_name: "Ada" }),
        ...block(2, { commit_id: "1".repeat(40), author_email: "", author_name: "Grace" }),
      ],
      NOW,
    );
    expect(timeline.authors).toBe(2);
  });

  it("reports the median age of the dated lines in whole days", () => {
    const timeline = buildBlameTimeline(
      [
        line({ timestamp: at(2026, 9, 18) }),
        line({ timestamp: at(2026, 9, 9) }),
        line({ timestamp: at(2026, 8, 20) }),
      ],
      NOW,
    );
    expect(timeline.medianAgeDays).toBe(10);
    expect(timeline.newestTimestamp).toBe(at(2026, 9, 18));
    expect(timeline.oldestTimestamp).toBe(at(2026, 8, 20));
  });

  it("reports no median when nothing is dated", () => {
    expect(buildBlameTimeline(block(2, { commit_id: ZERO }), NOW).medianAgeDays).toBeNull();
  });

  it("scales columns against the tallest period", () => {
    const timeline = buildBlameTimeline(
      [...block(90, { timestamp: at(2026, 9, 1) }), ...block(10, { timestamp: at(2026, 4, 1) })],
      NOW,
    );
    expect(timeline.peakPercent).toBeCloseTo(90, 9);
    const tallest = timeline.periods.find((period) => period.lines === 90);
    expect(columnHeight(tallest?.percent ?? 0, timeline.peakPercent, 36)).toBe(36);
  });
});

describe("share and column formatting", () => {
  it("never rounds a drawn share down to nothing", () => {
    expect(formatShare(0)).toBe("0%");
    expect(formatShare(-4)).toBe("0%");
    expect(formatShare(Number.NaN)).toBe("0%");
    expect(formatShare(0.04)).toBe("<0.1%");
    expect(formatShare(0.1)).toBe("0.1%");
    expect(formatShare(4.25)).toBe("4.3%");
    expect(formatShare(9.99)).toBe("10.0%");
    expect(formatShare(12.4)).toBe("12%");
    expect(formatShare(100)).toBe("100%");
  });

  it("keeps a period with lines visible and an empty one on the baseline", () => {
    expect(columnHeight(0, 50, 36)).toBe(1);
    // 0.2% against a 40% peak rounds to nothing; it must still be drawn.
    expect(columnHeight(0.2, 40, 36)).toBe(2);
    expect(columnHeight(50, 50, 36)).toBe(36);
    // Degenerate scales collapse to the baseline rather than to NaN pixels.
    expect(columnHeight(10, 0, 36)).toBe(1);
    expect(columnHeight(10, 50, 0)).toBe(1);
    expect(columnHeight(Number.NaN, 50, 36)).toBe(1);
  });

  it("describes a period for a screen reader, including an empty one", () => {
    const timeline = buildBlameTimeline(
      [...block(1, { timestamp: at(2026, 4, 1) }), ...block(3, { timestamp: at(2026, 9, 1) })],
      NOW,
    );
    const empty = timeline.periods.find((period) => period.lines === 0);
    expect(describePeriod(empty!)).toMatch(/no lines$/);
    const filled = timeline.periods.find((period) => period.lines === 3);
    expect(describePeriod(filled!)).toContain("75% of the file");
    expect(describePeriod(filled!)).toContain("3 lines from 1 commit");
  });
});

describe("the age scale has one owner", () => {
  it("bands ascend and end unbounded", () => {
    expect(AGE_BANDS).toHaveLength(4);
    for (let i = 1; i < AGE_BANDS.length; i += 1) {
      expect(AGE_BANDS[i].maxDays).toBeGreaterThan(AGE_BANDS[i - 1].maxDays);
    }
    expect(AGE_BANDS[AGE_BANDS.length - 1].maxDays).toBe(Number.POSITIVE_INFINITY);
  });

  it("places an age in the first band it fits, inclusive of the bound", () => {
    expect(ageBandForDays(0).id).toBe("fresh");
    expect(ageBandForDays(7).id).toBe("fresh");
    expect(ageBandForDays(7.1).id).toBe("recent");
    expect(ageBandForDays(30).id).toBe("recent");
    expect(ageBandForDays(90.5).id).toBe("old");
    expect(ageBandForDays(Number.NaN).id).toBe("old");
  });

  it("tints a row only when its age is knowable", () => {
    const fresh = AGE_BANDS[0];
    expect(blameRowTint(line({ timestamp: Math.floor(NOW / 1000) - 3600 }), NOW)).toBe(
      bandColor(fresh, "tint"),
    );
    expect(bandColor(fresh, "fill")).toBe(`rgba(${fresh.rgb}, ${fresh.fillAlpha})`);

    // The three cases that are not ages. An undated line is not an ancient
    // one, a clock-skewed line is not a fresh one, and a worktree line — which
    // git dates NOW — is not the freshest code in the file. Tinting any of
    // them would also disagree with the band filter, which excludes all three.
    expect(blameRowTint(line({ timestamp: 0 }), NOW)).toBe("transparent");
    expect(blameRowTint(line({ timestamp: Number.NaN }), NOW)).toBe("transparent");
    expect(blameRowTint(line({ timestamp: Math.floor(NOW / 1000) + 86_400 }), NOW)).toBe(
      "transparent",
    );
    expect(
      blameRowTint(line({ commit_id: ZERO, timestamp: Math.floor(NOW / 1000) }), NOW),
    ).toBe("transparent");
  });

  it("puts the band shares and the periods over the same population", () => {
    const lines = [
      ...block(40, { timestamp: at(2026, 9, 16) }), // < 7d
      ...block(25, { timestamp: at(2026, 9, 1) }), // < 30d
      ...block(15, { timestamp: at(2025, 1, 5) }), // older
      ...block(10, { commit_id: ZERO }),
      ...block(6, { timestamp: at(2030, 1, 1) }),
      ...block(4, { timestamp: 0 }),
    ];
    const timeline = buildBlameTimeline(lines, NOW);
    const byId = Object.fromEntries(timeline.bands.map((share) => [share.band.id, share.lines]));
    expect(byId).toEqual({ fresh: 40, recent: 25, settled: 0, old: 15 });

    // The legend keeps naming the whole scale even where a band is empty.
    expect(timeline.bands.map((share) => share.band.id)).toEqual(
      AGE_BANDS.map((band) => band.id),
    );

    // Both partitions cover exactly the file, so a reader can add either one
    // up and get 100% — and a clock-skewed line is in the `future` extra in
    // both, never in the freshest band.
    const extras = timeline.extras.reduce((sum, extra) => sum + extra.percent, 0);
    const bands = timeline.bands.reduce((sum, share) => sum + share.percent, 0);
    const periods = timeline.periods.reduce((sum, period) => sum + period.percent, 0);
    expect(bands + extras).toBeCloseTo(100, 9);
    expect(periods + extras).toBeCloseTo(100, 9);
  });

  it("tints a timeline column by its own place on the same scale", () => {
    const timeline = buildBlameTimeline(
      [...block(2, { timestamp: at(2024, 1, 5) }), ...block(2, { timestamp: at(2026, 9, 18) })],
      NOW,
    );
    expect(timeline.periods[timeline.periods.length - 1].band.id).toBe("fresh");
    expect(timeline.periods[0].band.id).toBe("old");
  });
});
