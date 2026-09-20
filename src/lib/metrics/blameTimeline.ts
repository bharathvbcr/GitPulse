/**
 * Per-file blame timeline: what share of a file's lines survives from when.
 *
 * Blame already answers "who wrote this line". It could not answer "how much
 * of this file is actually from last week, and how much has not been touched
 * since 2021" without the reader scrolling the whole gutter and estimating.
 * This turns the timestamps blame already carries into a chronological axis of
 * periods, each one labelled with its share of the file.
 *
 * Three rules shape it, and every one of them exists because the honest
 * alternative reads worse:
 *
 *  - **The denominator is the whole file.** Shares are `lines / totalLines`,
 *    counting uncommitted, undated and future-dated lines. A bar that summed
 *    to 100% of *some* of the file, with the rest silently excluded, would
 *    overstate every period it did draw.
 *  - **Lines a time axis cannot hold are named, not dropped.** Worktree-only
 *    lines have no commit date, a skewed clock can date a line after `now`,
 *    and a corrupt timestamp is not a date at all. Each gets its own labelled
 *    extra so the shares still add up to the file.
 *  - **The picture and the filter classify identically.** {@link blameBucketKey}
 *    is the same function the builder counts with, so a column that claims
 *    12% cannot select a different set of lines than it counted.
 *
 * Bucketing is by **local calendar periods**, which is what a reader means by
 * "March": [`./commitCadence`] owns the day boundary and this imports it
 * rather than keeping a second copy that could be corrected on one side only.
 */

import type { BlameLine } from "../files/types";
import { nextLocalDay, startOfLocalDay } from "./commitCadence";
// The rail is a map of a scrolling list, which is a problem the diff minimap
// already solved — how far down a pointer landed, where the viewport band
// sits, and what scroll offset CENTRES the row you aimed at. Those are
// list-shaped, not diff-shaped, so this imports them rather than deriving a
// second set that would drift on the two mistakes documented there.
import { MAX_TICKS, MIN_TICK_PCT } from "../diff/minimap";

/** Worktree-only lines carry an all-zero OID, at SHA-1 or SHA-256 length. */
const ZERO_OID_RE = /^0+$/;

const MS_PER_DAY = 86_400_000;

/**
 * Hard ceiling on drawn periods.
 *
 * The axis is picked to fit this, stepping to a coarser granularity rather
 * than drawing more columns. A repository old enough to overflow even yearly
 * periods folds its oldest lines into the leading column instead of growing
 * an unbounded array from an attacker-supplied (or merely wrong) timestamp.
 */
export const MAX_PERIODS = 48;

export type BlameGranularity = "day" | "week" | "month" | "quarter" | "year";

/** Finest first: the axis takes the first that fits {@link MAX_PERIODS}. */
const GRANULARITIES: readonly BlameGranularity[] = [
  "day",
  "week",
  "month",
  "quarter",
  "year",
];

/**
 * The age scale, declared once for the row tint, the column fill and the
 * legend.
 *
 * These thresholds are the blame page's own and are deliberately finer than
 * the repository-wide knowledge buckets in the Rust reader (30/90/365/730):
 * that metric answers "is this repository's knowledge concentrated", this one
 * answers "is the line under my cursor fresh". They are kept apart on purpose.
 * Within this page, though, one table drives all three surfaces — a legend
 * hand-listed beside a colour function is how a swatch comes to name a band
 * the rows no longer use.
 */
export interface AgeBand {
  readonly id: "fresh" | "recent" | "settled" | "old";
  /** Legend text, e.g. `< 7d`. */
  readonly label: string;
  /** Upper bound in days, exclusive of the next band; `Infinity` for the last. */
  readonly maxDays: number;
  /** `r, g, b` triple, so tint and fill cannot drift to different hues. */
  readonly rgb: string;
  /** Alpha for a blame row's background, behind source text. */
  readonly tintAlpha: number;
  /** Alpha for a timeline column and its legend swatch. */
  readonly fillAlpha: number;
}

export const AGE_BANDS: readonly AgeBand[] = [
  { id: "fresh", label: "< 7d", maxDays: 7, rgb: "239, 68, 68", tintAlpha: 0.2, fillAlpha: 0.7 },
  { id: "recent", label: "< 30d", maxDays: 30, rgb: "245, 158, 11", tintAlpha: 0.15, fillAlpha: 0.7 },
  { id: "settled", label: "< 90d", maxDays: 90, rgb: "59, 130, 246", tintAlpha: 0.12, fillAlpha: 0.7 },
  {
    id: "old",
    label: "Older",
    maxDays: Number.POSITIVE_INFINITY,
    rgb: "107, 114, 128",
    tintAlpha: 0.08,
    fillAlpha: 0.55,
  },
];

/** The band an age in days falls in. A negative age is the freshest band. */
export function ageBandForDays(days: number): AgeBand {
  const age = Number.isFinite(days) ? days : Number.POSITIVE_INFINITY;
  for (const band of AGE_BANDS) {
    if (age <= band.maxDays) return band;
  }
  return AGE_BANDS[AGE_BANDS.length - 1];
}

/** `rgba(...)` for a band at one of its two alphas. */
export function bandColor(band: AgeBand, kind: "tint" | "fill"): string {
  return `rgba(${band.rgb}, ${kind === "tint" ? band.tintAlpha : band.fillAlpha})`;
}

/**
 * The age band one line belongs to, or null when it has no knowable age.
 *
 * Null for the three cases that are not ages: a worktree line (no commit, and
 * git dates it *now*, so a timestamp here would read as the freshest code in
 * the file), a line whose author time is missing, and a line dated after now,
 * which a skewed clock produces and which is not "zero days old".
 *
 * This is the single classifier behind the row tint, the legend's per-band
 * shares, the age filter and the rail. A second spelling of "which band is
 * this line in" is how a legend comes to offer a filter that selects a
 * different set from the one it counted.
 */
export function bandOfLine(line: BlameLine, nowMs: number): AgeBand | null {
  if (isUncommittedLine(line)) return null;
  const seconds = datedSeconds(line);
  if (seconds === null) return null;
  const ms = seconds * 1000;
  if (ms > nowMs) return null;
  return ageBandForDays((nowMs - ms) / MS_PER_DAY);
}

/**
 * Background for one blame row.
 *
 * `transparent` for a line with no knowable age: an undated line is not an
 * ancient one, and a clock-skewed line is not a fresh one — tinting either
 * would say it was, and would disagree with the band filter that excludes it.
 */
export function blameRowTint(line: BlameLine, nowMs: number): string {
  const band = bandOfLine(line, nowMs);
  return band === null ? "transparent" : bandColor(band, "tint");
}

/** One period on the axis. Present even when it holds no lines. */
export interface BlamePeriod {
  readonly kind: "period";
  /** Stable across rebuilds of the same axis; the filter key. */
  readonly key: string;
  /** Human label, e.g. `Mar 2026`, `Q1 2026`, `Week of 2 Mar 2026`. */
  readonly label: string;
  /** Local period start, epoch ms, inclusive. */
  readonly start: number;
  /** Local start of the following period, epoch ms, exclusive. */
  readonly end: number;
  readonly lines: number;
  /** Share of every line in the file, 0..100, unrounded. */
  readonly percent: number;
  /** Distinct commits and authors among this period's committed lines. */
  readonly commits: number;
  readonly authors: number;
  /** Age band of the period's newest instant, for the column fill. */
  readonly band: AgeBand;
  /**
   * True on the leading period when the axis could not reach the oldest line
   * and everything before it was folded in. The column is then a floor for
   * that period, not a reading of it.
   */
  readonly foldedOlder: boolean;
}

/** A group of lines that a time axis cannot honestly hold. */
export interface BlameExtra {
  readonly kind: "extra";
  readonly key: "uncommitted" | "undated" | "future";
  readonly label: string;
  /** Why these lines are off the axis; shown as the chip's title. */
  readonly explanation: string;
  readonly lines: number;
  readonly percent: number;
}

/**
 * One age band's share of the file — the legend, with numbers on it.
 *
 * The legend used to be four static swatches restating a colour scale the
 * reader could already see. These carry the distribution and select on it, so
 * "how much of this file has not been touched in three months" is a glance and
 * a click rather than a scroll.
 */
export interface BandShare {
  readonly band: AgeBand;
  readonly lines: number;
  readonly percent: number;
}

export interface BlameTimeline {
  readonly granularity: BlameGranularity;
  /** Oldest first, contiguous, gaps included as empty periods. */
  readonly periods: readonly BlamePeriod[];
  /** Only the non-empty ones, so a clean file shows no chips at all. */
  readonly extras: readonly BlameExtra[];
  /**
   * Every band, in scale order, including the empty ones — the legend has to
   * keep naming the whole scale, or a file with nothing older than a month
   * would appear to have no "Older" band rather than an empty one.
   *
   * These and {@link BlameTimeline.periods} are two partitions of exactly the
   * same population, so `bands + extras` and `periods + extras` both come to
   * 100% of the file. A clock-skewed line sits in the `future` extra in both,
   * never in the freshest band.
   */
  readonly bands: readonly BandShare[];
  /** The instant the timeline was built against; classification depends on it. */
  readonly nowMs: number;
  /** Every blame line, and the denominator of every percentage here. */
  readonly totalLines: number;
  /** Lines placed on the axis. */
  readonly datedLines: number;
  /** Largest single-period share, and the scaling floor for a column chart. */
  readonly peakPercent: number;
  /** Distinct commits / authors across the file's committed lines. */
  readonly commits: number;
  readonly authors: number;
  /** Epoch seconds of the newest and oldest dated line, or null when none. */
  readonly newestTimestamp: number | null;
  readonly oldestTimestamp: number | null;
  /** Median age of the dated lines, in whole days. */
  readonly medianAgeDays: number | null;
}

const EMPTY_TIMELINE: BlameTimeline = {
  granularity: "month",
  periods: [],
  extras: [],
  bands: AGE_BANDS.map((band) => ({ band, lines: 0, percent: 0 })),
  nowMs: 0,
  totalLines: 0,
  datedLines: 0,
  peakPercent: 0,
  commits: 0,
  authors: 0,
  newestTimestamp: null,
  oldestTimestamp: null,
  medianAgeDays: null,
};

const MONTHS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
];

/** True for a worktree-only line, which has no commit and so no commit date. */
export function isUncommittedLine(line: BlameLine): boolean {
  return ZERO_OID_RE.test(line.commit_id ?? "");
}

/** Start of the local period containing `ms`. */
function startOfPeriod(ms: number, granularity: BlameGranularity): number {
  if (granularity === "day") return startOfLocalDay(ms);
  const date = new Date(ms);
  date.setHours(0, 0, 0, 0);
  switch (granularity) {
    case "week": {
      // Weeks start Monday, which is what "week of" means to most readers;
      // getDay() calls Sunday 0, so shift before subtracting.
      date.setDate(date.getDate() - ((date.getDay() + 6) % 7));
      break;
    }
    case "month":
      // setMonth(month, day) sets both at once. Setting the day separately is
      // what turns 31 March into 2 March when the month moves first.
      date.setMonth(date.getMonth(), 1);
      break;
    case "quarter":
      date.setMonth(Math.floor(date.getMonth() / 3) * 3, 1);
      break;
    case "year":
      date.setFullYear(date.getFullYear(), 0, 1);
      break;
  }
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

/** Start of the period after the one beginning at `start`. */
function nextPeriodStart(start: number, granularity: BlameGranularity): number {
  if (granularity === "day") return nextLocalDay(start);
  const date = new Date(start);
  switch (granularity) {
    case "week":
      date.setDate(date.getDate() + 7);
      break;
    case "month":
      date.setMonth(date.getMonth() + 1, 1);
      break;
    case "quarter":
      date.setMonth(date.getMonth() + 3, 1);
      break;
    case "year":
      date.setFullYear(date.getFullYear() + 1, 0, 1);
      break;
  }
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

function labelForPeriod(start: number, granularity: BlameGranularity): string {
  const date = new Date(start);
  const day = date.getDate();
  const month = MONTHS[date.getMonth()];
  const year = date.getFullYear();
  switch (granularity) {
    case "day":
      return `${day} ${month} ${year}`;
    case "week":
      return `Week of ${day} ${month} ${year}`;
    case "month":
      return `${month} ${year}`;
    case "quarter":
      return `Q${Math.floor(date.getMonth() / 3) + 1} ${year}`;
    case "year":
      return `${year}`;
  }
}

/**
 * Period starts from `firstMs` to `lastMs` inclusive, or null when that needs
 * more than `max` of them.
 *
 * Bounded by construction: the walk stops the moment it exceeds the cap, so a
 * line dated in 1901 costs one extra iteration per granularity rather than a
 * million-element array. A step that fails to advance also returns null — a
 * calendar edge must not spin this forever.
 */
function buildAxis(
  firstMs: number,
  lastMs: number,
  granularity: BlameGranularity,
  max: number,
): number[] | null {
  const end = startOfPeriod(lastMs, granularity);
  let cursor = startOfPeriod(firstMs, granularity);
  const starts: number[] = [];
  while (cursor <= end) {
    starts.push(cursor);
    if (starts.length > max) return null;
    const next = nextPeriodStart(cursor, granularity);
    if (!(next > cursor)) return null;
    cursor = next;
  }
  return starts.length > 0 ? starts : [end];
}

/** The newest `max` yearly periods ending at `lastMs`; the fallback axis. */
function clampedYearAxis(lastMs: number, max: number): number[] {
  const starts: number[] = [];
  let cursor = startOfPeriod(lastMs, "year");
  for (let i = 0; i < max; i += 1) {
    starts.unshift(cursor);
    const previous = new Date(cursor);
    previous.setFullYear(previous.getFullYear() - 1, 0, 1);
    previous.setHours(0, 0, 0, 0);
    cursor = previous.getTime();
  }
  return starts;
}

/** A usable author timestamp in epoch seconds, or null. */
function datedSeconds(line: BlameLine): number | null {
  const ts = line.timestamp;
  return Number.isFinite(ts) && ts > 0 ? ts : null;
}

/**
 * Bucket key for one line against an already-built axis.
 *
 * The builder counts with this and the UI filters with it, so a column's count
 * and the rows it selects are the same population by construction.
 */
function classify(
  line: BlameLine,
  periods: readonly { key: string; start: number; end: number }[],
  nowMs: number,
): string {
  if (isUncommittedLine(line)) return "uncommitted";
  const seconds = datedSeconds(line);
  if (seconds === null) return "undated";
  const ms = seconds * 1000;
  if (ms > nowMs) return "future";
  if (periods.length === 0) return "undated";
  // Older than the axis reaches: folded into the leading period, which is
  // reported as a floor rather than as a reading of that period alone.
  if (ms < periods[0].start) return periods[0].key;

  let low = 0;
  let high = periods.length - 1;
  while (low < high) {
    const mid = (low + high + 1) >> 1;
    if (periods[mid].start <= ms) low = mid;
    else high = mid - 1;
  }
  return periods[low].key;
}

/**
 * Bucket key for one line against a built timeline.
 *
 * Returns the key of the period or extra the line was counted in, so a caller
 * filtering on a selected bucket selects exactly the lines that bucket claims.
 */
export function blameBucketKey(line: BlameLine, timeline: BlameTimeline): string {
  return classify(line, timeline.periods, timeline.nowMs);
}

/**
 * What the gutter is filtered to: one period, one off-axis group, or one age
 * band. Three ways in — a column, a chip, a legend swatch — and one rule for
 * what each selects, so all three are checked by the same test.
 */
export type BlameSelection =
  | { readonly kind: "bucket"; readonly key: string }
  | { readonly kind: "band"; readonly id: AgeBand["id"] };

/** Whether `line` belongs to the current selection. */
export function selectionMatches(
  line: BlameLine,
  selection: BlameSelection,
  timeline: BlameTimeline,
): boolean {
  return selection.kind === "band"
    ? bandOfLine(line, timeline.nowMs)?.id === selection.id
    : blameBucketKey(line, timeline) === selection.key;
}

/** Whether two selections name the same thing — for toggling one off. */
export function sameSelection(a: BlameSelection | null, b: BlameSelection | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.kind === "band" && b.kind === "band") return a.id === b.id;
  if (a.kind === "bucket" && b.kind === "bucket") return a.key === b.key;
  return false;
}

/**
 * The label a selection shows in the "showing N of M" line.
 *
 * Resolved against the timeline that is drawn, so a selection naming something
 * this file does not have resolves to null and the caller shows the whole file
 * rather than filtering to nothing.
 */
export function selectionLabel(
  selection: BlameSelection | null,
  timeline: BlameTimeline,
): string | null {
  if (selection === null) return null;
  if (selection.kind === "band") {
    const share = timeline.bands.find((entry) => entry.band.id === selection.id);
    return share && share.lines > 0 ? `lines ${share.band.label}` : null;
  }
  const period = timeline.periods.find((entry) => entry.key === selection.key);
  if (period) return period.label;
  const extra = timeline.extras.find((entry) => entry.key === selection.key);
  return extra ? extra.label : null;
}

const EXTRA_META: Record<
  BlameExtra["key"],
  { label: string; explanation: string }
> = {
  uncommitted: {
    label: "Uncommitted",
    explanation:
      "Worktree-only lines. They have no commit, so no commit date to place on the axis.",
  },
  future: {
    label: "Future-dated",
    explanation:
      "Lines whose author date is later than now, which a skewed clock can produce. Counted here rather than placed in a period that has not happened.",
  },
  undated: {
    label: "Undated",
    explanation:
      "Committed lines whose author timestamp is missing or unusable, so their age is unknown.",
  },
};

/** Extras in the order they are drawn: newest-flavoured first. */
const EXTRA_ORDER: readonly BlameExtra["key"][] = ["uncommitted", "future", "undated"];

/**
 * Build the timeline for one file's blame output.
 *
 * @param lines Every blame line for the file, in any order.
 * @param nowMs The instant to measure ages against. Injected so this is pure
 *   and so the picture, the row tints and the filter all agree on "now".
 */
export function buildBlameTimeline(
  lines: readonly BlameLine[],
  nowMs: number,
): BlameTimeline {
  if (!Number.isFinite(nowMs) || lines.length === 0) {
    return { ...EMPTY_TIMELINE, nowMs: Number.isFinite(nowMs) ? nowMs : 0 };
  }

  const totalLines = lines.length;
  const dated: number[] = [];
  let oldestMs = Number.POSITIVE_INFINITY;
  let newestMs = Number.NEGATIVE_INFINITY;
  for (const line of lines) {
    if (isUncommittedLine(line)) continue;
    const seconds = datedSeconds(line);
    if (seconds === null) continue;
    const ms = seconds * 1000;
    if (ms > nowMs) continue;
    dated.push(seconds);
    if (ms < oldestMs) oldestMs = ms;
    if (ms > newestMs) newestMs = ms;
  }

  // The axis always runs to now, so "nothing has been touched in a year" is
  // visible as empty columns rather than as an axis that simply stops. With no
  // dated line at all there is no span to draw, and one empty column labelled
  // today would imply the file has a history on the axis that it does not.
  const axisEnd = nowMs;
  const axisStart = Number.isFinite(oldestMs) ? Math.min(oldestMs, axisEnd) : axisEnd;
  const hasAxis = dated.length > 0;

  let granularity: BlameGranularity = "year";
  let starts: number[] | null = null;
  if (hasAxis) {
    for (const candidate of GRANULARITIES) {
      const built = buildAxis(axisStart, axisEnd, candidate, MAX_PERIODS);
      if (built) {
        granularity = candidate;
        starts = built;
        break;
      }
    }
  }
  const clamped = hasAxis && starts === null;
  // Bound to a const before it is read inside a closure: a narrowed `let` does
  // not stay narrowed across one.
  const axis: readonly number[] = hasAxis
    ? (starts ?? clampedYearAxis(axisEnd, MAX_PERIODS))
    : [];
  const step = granularity;

  const bounds = axis.map((start, index) => ({
    key: `${step}:${start}`,
    start,
    end: index + 1 < axis.length ? axis[index + 1] : nextPeriodStart(start, step),
  }));

  const counts = new Map<string, number>();
  const bandCounts = new Map<AgeBand["id"], number>();
  const commitsByBucket = new Map<string, Set<string>>();
  const authorsByBucket = new Map<string, Set<string>>();
  const allCommits = new Set<string>();
  const allAuthors = new Set<string>();

  for (const line of lines) {
    const key = classify(line, bounds, nowMs);
    counts.set(key, (counts.get(key) ?? 0) + 1);
    const band = bandOfLine(line, nowMs);
    if (band) bandCounts.set(band.id, (bandCounts.get(band.id) ?? 0) + 1);
    if (isUncommittedLine(line)) continue;
    allCommits.add(line.commit_id);
    allAuthors.add(authorKey(line));
    let commits = commitsByBucket.get(key);
    if (!commits) commitsByBucket.set(key, (commits = new Set()));
    commits.add(line.commit_id);
    let authors = authorsByBucket.get(key);
    if (!authors) authorsByBucket.set(key, (authors = new Set()));
    authors.add(authorKey(line));
  }

  const share = (count: number) => (count / totalLines) * 100;

  const periods: BlamePeriod[] = bounds.map((bound, index) => {
    const count = counts.get(bound.key) ?? 0;
    const ageDays = Math.max(0, (nowMs - (bound.end - 1)) / MS_PER_DAY);
    return {
      kind: "period",
      key: bound.key,
      label: labelForPeriod(bound.start, granularity),
      start: bound.start,
      end: bound.end,
      lines: count,
      percent: share(count),
      commits: commitsByBucket.get(bound.key)?.size ?? 0,
      authors: authorsByBucket.get(bound.key)?.size ?? 0,
      band: ageBandForDays(ageDays),
      foldedOlder: index === 0 && clamped && count > 0,
    };
  });

  const extras: BlameExtra[] = [];
  for (const key of EXTRA_ORDER) {
    const count = counts.get(key) ?? 0;
    if (count === 0) continue;
    extras.push({
      kind: "extra",
      key,
      label: EXTRA_META[key].label,
      explanation: EXTRA_META[key].explanation,
      lines: count,
      percent: share(count),
    });
  }

  const bands: BandShare[] = AGE_BANDS.map((band) => {
    const count = bandCounts.get(band.id) ?? 0;
    return { band, lines: count, percent: share(count) };
  });

  dated.sort((a, b) => a - b);
  const medianSeconds = dated.length > 0 ? dated[Math.floor(dated.length / 2)] : null;

  return {
    granularity,
    periods,
    extras,
    bands,
    nowMs,
    totalLines,
    datedLines: dated.length,
    peakPercent: periods.reduce((peak, period) => Math.max(peak, period.percent), 0),
    commits: allCommits.size,
    authors: allAuthors.size,
    newestTimestamp: Number.isFinite(newestMs) ? Math.floor(newestMs / 1000) : null,
    oldestTimestamp: Number.isFinite(oldestMs) ? Math.floor(oldestMs / 1000) : null,
    medianAgeDays:
      medianSeconds === null
        ? null
        : Math.max(0, Math.floor((nowMs - medianSeconds * 1000) / MS_PER_DAY)),
  };
}

/**
 * Identity for counting distinct authors.
 *
 * Email first because a person commits under several spellings of their name
 * from different machines; the name is the fallback for history written
 * without one.
 */
function authorKey(line: BlameLine): string {
  const email = (line.author_email ?? "").trim().toLowerCase();
  return email !== "" ? email : (line.author_name ?? "").trim().toLowerCase();
}

/**
 * A share as text.
 *
 * A period holding a handful of lines in a large file is a real reading, and
 * rounding it to `0%` next to a drawn column says the column is a lie. Below a
 * tenth of a percent the honest statement is the bound, not a number.
 */
export function formatShare(percent: number): string {
  if (!Number.isFinite(percent) || percent <= 0) return "0%";
  if (percent < 0.1) return "<0.1%";
  if (percent < 10) return `${percent.toFixed(1)}%`;
  return `${Math.round(percent)}%`;
}

/**
 * Column height in px for a share, scaled against the tallest period.
 *
 * A period with lines never collapses to the empty baseline: at a 40:1 ratio
 * its true height rounds to nothing, and an invisible column reads as "no
 * lines here", which is the one thing it must not say.
 */
export function columnHeight(percent: number, peakPercent: number, maxPx: number): number {
  if (!(percent > 0)) return 1;
  if (!(peakPercent > 0) || !(maxPx > 0)) return 1;
  const scaled = (percent / peakPercent) * maxPx;
  return Math.max(2, Math.min(maxPx, Math.round(scaled)));
}

/**
 * How the axis names its own resolution, e.g. `daily`.
 *
 * Spelled out rather than built by suffixing the granularity, which reads
 * "dayly" for the one case a reader is most likely to see on a young file.
 */
const GRANULARITY_LABELS: Record<BlameGranularity, string> = {
  day: "daily",
  week: "weekly",
  month: "monthly",
  quarter: "quarterly",
  year: "yearly",
};

export function granularityLabel(granularity: BlameGranularity): string {
  return GRANULARITY_LABELS[granularity];
}

/**
 * The age rail: the whole file's age, as one heat strip beside the gutter.
 *
 * The timeline answers "how much of this file is recent"; it cannot answer
 * "*where* is it". Scrolling was the only way to find out, and a virtual list
 * only ever has a screenful rendered, so nothing on screen could show it. The
 * rail projects every line of the list being drawn onto one column.
 *
 * Two rules, both borrowed from the diff minimap that taught them:
 *
 *  - It maps the list ACTUALLY drawn. Hand it the filtered lines when a filter
 *    is on, or every tick points at a row that is not where the map says.
 *  - The freshest tone in a bucket wins, because a bucket holding one fresh
 *    line among sixty old ones is exactly what a reader opens the map to find.
 *    Painting it "old" would hide it. The intensity then says how much of the
 *    bucket that tone really is, so one line among sixty reads as a faint mark
 *    rather than as a solid block of new code.
 */
export type AgeTone = AgeBand["id"] | "uncommitted" | "undated";

export interface AgeTick {
  readonly key: string;
  readonly topPct: number;
  readonly heightPct: number;
  readonly tone: AgeTone;
  /** Share of the bucket carrying `tone`, 0–1; the heat of the mark. */
  readonly weight: number;
}

/** Freshest first. A worktree edit outranks every commit: it is the newest. */
const TONE_ORDER: readonly AgeTone[] = [
  "uncommitted",
  ...AGE_BANDS.map((band) => band.id),
  "undated",
];

/**
 * Colour for a rail tick.
 *
 * Uncommitted rides the themeable accent rather than a fifth fixed hue: it is
 * "yours, now", which is what the accent means everywhere else in the app.
 */
export function toneColor(tone: AgeTone, weight: number): string {
  const heat = Math.max(0.35, Math.min(1, Number.isFinite(weight) ? weight : 1));
  if (tone === "uncommitted") return `rgb(var(--c-accent) / ${heat.toFixed(3)})`;
  if (tone === "undated") return `rgba(107, 114, 128, ${(heat * 0.5).toFixed(3)})`;
  const band = AGE_BANDS.find((entry) => entry.id === tone) ?? AGE_BANDS[AGE_BANDS.length - 1];
  return `rgba(${band.rgb}, ${(heat * band.fillAlpha).toFixed(3)})`;
}

/** The tone one line carries on the rail. */
function toneOfLine(line: BlameLine, nowMs: number): AgeTone {
  if (isUncommittedLine(line)) return "uncommitted";
  return bandOfLine(line, nowMs)?.id ?? "undated";
}

/**
 * Bucket `lines` into at most `maxTicks` marks down the rail.
 *
 * @param lines    The list being drawn, in display order.
 * @param nowMs    The instant ages are measured against.
 * @param maxTicks Upper bound on marks. A few hundred pixels of rail cannot
 *   show more, and fewer would lose a single fresh line in a large file.
 */
export function buildAgeTicks(
  lines: readonly BlameLine[],
  nowMs: number,
  maxTicks = MAX_TICKS,
): AgeTick[] {
  const total = lines.length;
  if (total === 0 || !(maxTicks > 0) || !Number.isFinite(nowMs)) return [];
  const step = Math.max(1, Math.ceil(total / maxTicks));
  const ticks: AgeTick[] = [];
  for (let start = 0; start < total; start += step) {
    const end = Math.min(total, start + step);
    const counts = new Map<AgeTone, number>();
    for (let i = start; i < end; i += 1) {
      const tone = toneOfLine(lines[i], nowMs);
      counts.set(tone, (counts.get(tone) ?? 0) + 1);
    }
    const tone = TONE_ORDER.find((candidate) => (counts.get(candidate) ?? 0) > 0);
    if (!tone) continue;
    ticks.push({
      key: `a${start}`,
      topPct: (start / total) * 100,
      heightPct: Math.max(MIN_TICK_PCT, ((end - start) / total) * 100),
      tone,
      weight: (counts.get(tone) ?? 0) / (end - start),
    });
  }
  return ticks;
}

/** Screen-reader sentence for one period's column. */
export function describePeriod(period: BlamePeriod): string {
  if (period.lines === 0) return `${period.label}: no lines`;
  const noun = period.lines === 1 ? "line" : "lines";
  const floor = period.foldedOlder ? " or earlier" : "";
  return (
    `${period.label}${floor}: ${formatShare(period.percent)} of the file, ` +
    `${period.lines} ${noun} from ${period.commits} ` +
    `${period.commits === 1 ? "commit" : "commits"}`
  );
}
