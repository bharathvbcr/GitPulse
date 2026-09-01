/**
 * Commits-per-day bucketing for the cadence sparkline.
 *
 * Backlog C3. The graph store already holds every loaded commit with an author
 * timestamp, so repository rhythm costs nothing extra to show.
 *
 * Buckets are **local calendar days**, not fixed 86 400-second windows. A user
 * reading "yesterday" means the calendar day they lived through, which is 23 or
 * 25 hours long across a DST transition. Dividing epoch seconds by 86 400 would
 * silently shift every bucket boundary after such a transition.
 */

/** One day's bucket. */
export interface CadenceBucket {
  /** Local midnight starting the day, in epoch milliseconds. */
  readonly dayStart: number;
  /** `YYYY-MM-DD` in local time, stable for labelling and keys. */
  readonly day: string;
  readonly count: number;
}

export interface CadenceSummary {
  readonly buckets: readonly CadenceBucket[];
  readonly total: number;
  /** Highest single-day count, and the floor for scaling a sparkline. */
  readonly peak: number;
  /** Commits per day across the covered span, or 0 when nothing is covered. */
  readonly mean: number;
  /**
   * True when the history is shorter than the requested window, so a caller
   * can render the real span rather than padding it with fake empty days.
   */
  readonly partial: boolean;
}

/** A commit only needs its author timestamp, in epoch **seconds**. */
export interface CadenceCommit {
  readonly timestamp: number;
}

/** Guards against a runaway window turning into an unbounded array. */
export const MAX_BUCKETS = 366;

function startOfLocalDay(ms: number): number {
  const date = new Date(ms);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

function localDayKey(dayStart: number): string {
  const date = new Date(dayStart);
  const month = `${date.getMonth() + 1}`.padStart(2, "0");
  const day = `${date.getDate()}`.padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}

/** Step forward exactly one calendar day, which is not always 24 hours. */
function nextLocalDay(dayStart: number): number {
  const date = new Date(dayStart);
  date.setDate(date.getDate() + 1);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

function isUsable(commit: CadenceCommit): boolean {
  const { timestamp } = commit;
  return Number.isFinite(timestamp) && timestamp > 0;
}

/**
 * Bucket commits into consecutive local days ending at `now`.
 *
 * @param commits    Commits in any order; only the timestamp is read.
 * @param days       Window length in days. Clamped to [1, {@link MAX_BUCKETS}].
 * @param nowMs      End of the window, epoch ms. Injected so this stays pure.
 * @returns Buckets oldest-first, one per day, including days with no commits.
 */
export function bucketCommitsByDay(
  commits: readonly CadenceCommit[],
  days: number,
  nowMs: number,
): CadenceSummary {
  const empty: CadenceSummary = { buckets: [], total: 0, peak: 0, mean: 0, partial: true };
  if (!Number.isFinite(nowMs)) return empty;

  const window = Math.max(1, Math.min(MAX_BUCKETS, Math.floor(Number.isFinite(days) ? days : 1)));

  // Build the day axis by stepping calendar days, so a DST transition inside
  // the window does not shift later boundaries.
  const todayStart = startOfLocalDay(nowMs);
  const starts: number[] = [todayStart];
  for (let i = 1; i < window; i += 1) {
    const previous = new Date(starts[0]);
    previous.setDate(previous.getDate() - 1);
    previous.setHours(0, 0, 0, 0);
    starts.unshift(previous.getTime());
  }

  const counts = new Map<number, number>();
  let total = 0;
  const windowStart = starts[0];
  const windowEnd = nextLocalDay(todayStart);

  for (const commit of commits) {
    if (!isUsable(commit)) continue;
    const ms = commit.timestamp * 1000;
    // A commit dated in the future, or older than the window, is not counted;
    // clamping it into an edge bucket would invent activity that never happened.
    if (ms < windowStart || ms >= windowEnd) continue;
    const dayStart = startOfLocalDay(ms);
    counts.set(dayStart, (counts.get(dayStart) ?? 0) + 1);
    total += 1;
  }

  const buckets: CadenceBucket[] = starts.map((dayStart) => ({
    dayStart,
    day: localDayKey(dayStart),
    count: counts.get(dayStart) ?? 0,
  }));

  const peak = buckets.reduce((max, bucket) => Math.max(max, bucket.count), 0);

  // "Partial" means the history does not reach back across the whole window,
  // so the leading empty days reflect absent data rather than a quiet period.
  const oldestCommitMs = commits
    .filter(isUsable)
    .reduce((oldest, commit) => Math.min(oldest, commit.timestamp * 1000), Number.POSITIVE_INFINITY);
  const partial = !Number.isFinite(oldestCommitMs) || oldestCommitMs > windowStart;

  return {
    buckets,
    total,
    peak,
    mean: total / buckets.length,
    partial,
  };
}

/**
 * Sparkline heights as fractions of the peak, in bucket order.
 *
 * A day with no commits is 0 and a day at the peak is 1. With no commits at
 * all every height is 0, so a caller renders a flat baseline rather than
 * dividing by zero.
 */
export function sparklineHeights(summary: CadenceSummary): number[] {
  if (summary.peak <= 0) return summary.buckets.map(() => 0);
  return summary.buckets.map((bucket) => bucket.count / summary.peak);
}

/** Days per bucket count, for the "quiet since" style summary line. */
export function activeDayCount(summary: CadenceSummary): number {
  return summary.buckets.reduce((count, bucket) => count + (bucket.count > 0 ? 1 : 0), 0);
}
