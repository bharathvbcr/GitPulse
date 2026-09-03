/**
 * Pure calculation and bucketing functions for Pulse repository metrics.
 *
 * All time-based calculations take an explicit `nowMs` parameter to guarantee
 * deterministic behavior and 100% reproducible test assertions.
 */

import type { CoverageReport } from "../coverage/types";
import type {
  CommitWindow,
  HeatmapDay,
  HeatmapWeek,
  HotspotRiskItem,
  HygieneStats,
  LocTrendPoint,
  PeriodCompareDeltas,
  PulseCommitSummary,
  PulseFileChurn,
  PunchCardCell,
  PunchCardStats,
  RhythmStats,
  WeeklyLineBucket,
} from "./types";

export function startOfLocalDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** Step forward exactly one calendar day, which is not always 24 hours. */
export function nextLocalDay(dayStart: number): number {
  const date = new Date(dayStart);
  date.setDate(date.getDate() + 1);
  date.setHours(0, 0, 0, 0);
  return date.getTime();
}

export function formatLocalDayKey(ms: number): string {
  const d = new Date(ms);
  const year = d.getFullYear();
  const month = `${d.getMonth() + 1}`.padStart(2, "0");
  const day = `${d.getDate()}`.padStart(2, "0");
  return `${year}-${month}-${day}`;
}

/** Exclusive count of local calendar days strictly between two midnights. */
export function calendarDaysBetween(startMs: number, endMs: number): number {
  const start = startOfLocalDay(startMs);
  const end = startOfLocalDay(endMs);
  if (end <= start) return 0;
  let n = 0;
  let cursor = nextLocalDay(start);
  while (cursor < end) {
    n += 1;
    cursor = nextLocalDay(cursor);
  }
  return n;
}

/**
 * Mirror of the canonical header grammar in `analyzer/conventional.rs`
 * (`ConventionalCommitParser::header_regex`).
 *
 * That parser is the single owner of what "conventional" means, and it accepts
 * ANY alphabetic type — not a fixed vocabulary — plus any non-empty scope and
 * an empty description. Encoding a stricter rule here made Pulse disagree with
 * the commit badges and the `type:` filter about the same commit: a repo using
 * `wip:` or `hotfix:` counted as conventional everywhere except this metric,
 * and a scope containing a space (`fix(build system): x`) counted nowhere else.
 *
 * `scripts/conventional-grammar-contract.test.ts` re-derives this pattern from
 * the Rust source, so the two cannot drift apart silently again.
 */
const CONVENTIONAL_RE = /^[a-zA-Z]+(?:\([^)]+\))?!?:\s*.*$/;

export function isConventionalCommit(subject: string): boolean {
  // The Rust parser matches its anchored pattern against `lines.next()`, so a
  // full message with a body still parses on its header. Testing the whole
  // string here would reject exactly the commits that carry trailers.
  return CONVENTIONAL_RE.test(headerLine(subject));
}

/** First line, trimmed — the `raw_message.lines().next()?.trim()` of the Rust parser. */
export function headerLine(message: string): string {
  const newline = message.indexOf("\n");
  return (newline === -1 ? message : message.slice(0, newline)).trim();
}

/**
 * Computes a 53-week contribution calendar grid ending on Saturday of the current week.
 *
 * @param commits Commit list
 * @param weeksCount Number of weeks (default 53)
 * @param nowMs Injected current timestamp
 * @param mode Weighting mode: 'count' (commit count) or 'churn' (lines changed)
 */
export function computeHeatmap(
  commits: readonly PulseCommitSummary[],
  weeksCount = 53,
  nowMs = Date.now(),
  mode: "count" | "churn" = "count",
): readonly HeatmapWeek[] {
  if (!Number.isFinite(nowMs) || weeksCount <= 0) return [];

  const now = new Date(nowMs);
  // Saturday ending the current week:
  const dayOfWeek = now.getDay(); // 0 = Sun .. 6 = Sat
  const endSaturday = new Date(now);
  endSaturday.setDate(now.getDate() + (6 - dayOfWeek));
  endSaturday.setHours(23, 59, 59, 999);

  // Total calendar days to span: weeksCount * 7
  const totalDays = weeksCount * 7;
  const startSunday = new Date(endSaturday);
  startSunday.setDate(endSaturday.getDate() - totalDays + 1);
  startSunday.setHours(0, 0, 0, 0);

  const startMs = startSunday.getTime();
  const endMs = endSaturday.getTime();

  // Aggregate commits by local day key
  const dayAggregates = new Map<
    string,
    { count: number; additions: number; deletions: number; churn: number }
  >();

  for (const commit of commits) {
    const commitMs = commit.timestamp * 1000;
    if (commitMs < startMs || commitMs > endMs) continue;
    const key = formatLocalDayKey(commitMs);
    const existing = dayAggregates.get(key) ?? { count: 0, additions: 0, deletions: 0, churn: 0 };
    const churn = commit.additions + commit.deletions;
    dayAggregates.set(key, {
      count: existing.count + 1,
      additions: existing.additions + commit.additions,
      deletions: existing.deletions + commit.deletions,
      churn: existing.churn + churn,
    });
  }

  // Find max value across all days for dynamic quantile thresholds
  const values: number[] = [];
  for (const agg of dayAggregates.values()) {
    const val = mode === "churn" ? agg.churn : agg.count;
    if (val > 0) values.push(val);
  }
  values.sort((a, b) => a - b);

  const peak = values[values.length - 1] ?? 0;

  // Scale against the observed peak so the darkest cell is reachable.
  // Quartiles of a short series collapse onto the max (`<= q3` never
  // produced level 4 on a 4-day calendar).
  function calculateLevel(value: number): number {
    if (value <= 0 || peak <= 0) return 0;
    const t = value / peak;
    if (t > 0.75) return 4;
    if (t > 0.5) return 3;
    if (t > 0.25) return 2;
    return 1;
  }

  const weeks: HeatmapWeek[] = [];
  let currentCursor = new Date(startSunday);

  for (let w = 0; w < weeksCount; w++) {
    const days: HeatmapDay[] = [];
    for (let d = 0; d < 7; d++) {
      const curMs = currentCursor.getTime();
      const dateKey = formatLocalDayKey(curMs);
      const agg = dayAggregates.get(dateKey) ?? { count: 0, additions: 0, deletions: 0, churn: 0 };
      const val = mode === "churn" ? agg.churn : agg.count;

      days.push({
        date: dateKey,
        dayOfWeek: currentCursor.getDay(),
        timestamp: curMs,
        count: agg.count,
        additions: agg.additions,
        deletions: agg.deletions,
        churn: agg.churn,
        level: calculateLevel(val),
      });

      currentCursor.setDate(currentCursor.getDate() + 1);
    }
    weeks.push({ weekIndex: w, days });
  }

  return weeks;
}

/**
 * Computes rhythm and streak metrics using local calendar days.
 */
export function computeRhythm(
  commits: readonly PulseCommitSummary[],
  windowDays = 90,
  nowMs = Date.now(),
): RhythmStats {
  if (!commits || commits.length === 0 || !Number.isFinite(nowMs)) {
    return {
      currentStreak: 0,
      longestStreak: 0,
      activeDaysInWindow: 0,
      totalDaysInWindow: Math.max(1, windowDays),
      longestInactiveGap: 0,
    };
  }

  // Collect unique active local days as sorted epoch ms (at local start of day)
  const activeDaysSet = new Set<number>();
  for (const commit of commits) {
    if (commit.timestamp > 0) {
      activeDaysSet.add(startOfLocalDay(commit.timestamp * 1000));
    }
  }

  const sortedDays = Array.from(activeDaysSet).sort((a, b) => a - b);
  if (sortedDays.length === 0) {
    return {
      currentStreak: 0,
      longestStreak: 0,
      activeDaysInWindow: 0,
      totalDaysInWindow: Math.max(1, windowDays),
      longestInactiveGap: 0,
    };
  }

  const todayStart = startOfLocalDay(nowMs);
  const windowStartDate = new Date(todayStart);
  windowStartDate.setDate(windowStartDate.getDate() - (Math.max(1, windowDays) - 1));
  const windowStart = startOfLocalDay(windowStartDate.getTime());

  // Active days in window
  const inWindow = sortedDays.filter((day) => day >= windowStart && day <= todayStart);
  const activeInWindow = inWindow.length;

  // Current streak (counting backward from today). May extend past the
  // window — that is the meaning of "current".
  let currentStreak = 0;
  let probe = todayStart;

  if (!activeDaysSet.has(probe)) {
    probe = startOfLocalDay(new Date(probe).setDate(new Date(probe).getDate() - 1));
  }

  while (activeDaysSet.has(probe)) {
    currentStreak++;
    probe = startOfLocalDay(new Date(probe).setDate(new Date(probe).getDate() - 1));
  }

  // Longest streak and longest inactive gap *inside the same window* as
  // activeDaysInWindow, so the card is one coherent scope.
  let longestStreak = 0;
  let streakAcc = 0;
  let lastDay: number | null = null;
  let longestGap = 0;

  for (const day of inWindow) {
    if (lastDay === null) {
      streakAcc = 1;
    } else {
      const nextExpectedMs = nextLocalDay(lastDay);
      if (day === nextExpectedMs) {
        streakAcc++;
      } else {
        const gapDays = calendarDaysBetween(lastDay, day);
        if (gapDays > longestGap) longestGap = gapDays;
        streakAcc = 1;
      }
    }
    if (streakAcc > longestStreak) {
      longestStreak = streakAcc;
    }
    lastDay = day;
  }

  if (lastDay !== null) {
    const trailing = calendarDaysBetween(lastDay, todayStart);
    if (trailing > longestGap) longestGap = trailing;
  }

  return {
    currentStreak,
    longestStreak,
    activeDaysInWindow: activeInWindow,
    totalDaysInWindow: windowDays,
    longestInactiveGap: longestGap,
  };
}

/**
 * Computes punch card metrics (7 days of week x 24 hours of day).
 */
export function computePunchCard(commits: readonly PulseCommitSummary[]): PunchCardStats {
  const map = new Map<string, { count: number; churn: number }>();

  let maxCount = 0;
  let maxChurn = 0;
  let afterHoursCommits = 0;
  let totalUsable = 0;

  for (const commit of commits) {
    if (commit.timestamp <= 0) continue;
    totalUsable++;
    const date = new Date(commit.timestamp * 1000);
    const day = date.getDay(); // 0 = Sun .. 6 = Sat
    const hour = date.getHours(); // 0 .. 23
    const churn = commit.additions + commit.deletions;

    // After hours = weekends (0 or 6) OR weekday hours < 9 or >= 18
    const isAfterHours = day === 0 || day === 6 || hour < 9 || hour >= 18;
    if (isAfterHours) {
      afterHoursCommits++;
    }

    const key = `${day}:${hour}`;
    const existing = map.get(key) ?? { count: 0, churn: 0 };
    const updated = { count: existing.count + 1, churn: existing.churn + churn };
    map.set(key, updated);

    if (updated.count > maxCount) maxCount = updated.count;
    if (updated.churn > maxChurn) maxChurn = updated.churn;
  }

  const cells: PunchCardCell[] = [];
  for (let d = 0; d < 7; d++) {
    for (let h = 0; h < 24; h++) {
      const entry = map.get(`${d}:${h}`);
      cells.push({
        dayOfWeek: d,
        hour: h,
        count: entry?.count ?? 0,
        churn: entry?.churn ?? 0,
      });
    }
  }

  const afterHoursPercentage =
    totalUsable > 0 ? Math.round((afterHoursCommits / totalUsable) * 100) : 0;

  return {
    cells,
    maxCount,
    maxChurn,
    totalCommits: totalUsable,
    afterHoursCommits,
    afterHoursPercentage,
  };
}

/**
 * Computes weekly lines added and deleted history.
 */
export function computeLineChanges(
  commits: readonly PulseCommitSummary[],
  weeksCount = 26,
  nowMs = Date.now(),
): readonly WeeklyLineBucket[] {
  if (weeksCount <= 0 || !Number.isFinite(nowMs)) return [];

  const now = new Date(nowMs);
  const dayOfWeek = now.getDay();
  // Start of current week Sunday:
  const currentSunday = new Date(now);
  currentSunday.setDate(now.getDate() - dayOfWeek);
  currentSunday.setHours(0, 0, 0, 0);

  const buckets: WeeklyLineBucket[] = [];
  const weekStarts: number[] = [];

  for (let i = weeksCount - 1; i >= 0; i--) {
    const s = new Date(currentSunday);
    s.setDate(currentSunday.getDate() - i * 7);
    weekStarts.push(s.getTime());
  }

  const lastWeekStart = weekStarts[weekStarts.length - 1];
  const windowEnd = nextLocalDay(
    startOfLocalDay(new Date(lastWeekStart).setDate(new Date(lastWeekStart).getDate() + 6)),
  );
  const minMs = weekStarts[0];

  const addsMap = new Map<number, number>();
  const delsMap = new Map<number, number>();

  for (const commit of commits) {
    const commitMs = commit.timestamp * 1000;
    if (commitMs < minMs || commitMs >= windowEnd) continue;

    // Largest weekStart <= commitMs. Calendar week starts, not a fixed
    // 7×24h divisor — that drifts an hour across DST and lands boundary
    // commits in the wrong column (see commitCadence.ts).
    let bucketStart = weekStarts[0];
    for (let i = weekStarts.length - 1; i >= 0; i--) {
      if (commitMs >= weekStarts[i]) {
        bucketStart = weekStarts[i];
        break;
      }
    }
    addsMap.set(bucketStart, (addsMap.get(bucketStart) ?? 0) + commit.additions);
    delsMap.set(bucketStart, (delsMap.get(bucketStart) ?? 0) + commit.deletions);
  }

  for (const start of weekStarts) {
    const additions = addsMap.get(start) ?? 0;
    const deletions = delsMap.get(start) ?? 0;
    buckets.push({
      weekStart: formatLocalDayKey(start),
      timestamp: start,
      additions,
      deletions,
      net: additions - deletions,
    });
  }

  return buckets;
}

/**
 * Reconstructs the historical LOC progression by walking backwards from today's current LOC.
 */
export function computeLocTrend(
  currentTotalLoc: number,
  commits: readonly PulseCommitSummary[],
): readonly LocTrendPoint[] {
  if (commits.length === 0 || currentTotalLoc <= 0) return [];

  // Group commits by day
  const dailyDelta = new Map<string, { timestamp: number; net: number }>();

  for (const commit of commits) {
    if (commit.timestamp <= 0) continue;
    const key = formatLocalDayKey(commit.timestamp * 1000);
    const net = commit.additions - commit.deletions;
    const existing = dailyDelta.get(key) ?? { timestamp: commit.timestamp * 1000, net: 0 };
    dailyDelta.set(key, { timestamp: existing.timestamp, net: existing.net + net });
  }

  // Sort dates chronologically
  const sortedKeys = Array.from(dailyDelta.keys()).sort();
  if (sortedKeys.length === 0) return [];

  // Compute total net change across all scanned history
  let totalNet = 0;
  for (const key of sortedKeys) {
    totalNet += dailyDelta.get(key)!.net;
  }

  // Starting baseline at beginning of scanned window
  let runningLoc = Math.max(0, currentTotalLoc - totalNet);
  const trend: LocTrendPoint[] = [];

  for (const key of sortedKeys) {
    const item = dailyDelta.get(key)!;
    runningLoc = Math.max(0, runningLoc + item.net);
    trend.push({
      date: key,
      timestamp: item.timestamp,
      totalLoc: runningLoc,
    });
  }

  return trend;
}

/**
 * Computes commit hygiene and quality indicators.
 */
export function computeHygiene(commits: readonly PulseCommitSummary[]): HygieneStats {
  if (!commits || commits.length === 0) {
    return {
      totalCommits: 0,
      conventionalCount: 0,
      conventionalPercentage: 0,
      signedCount: 0,
      signedPercentage: 0,
      mergeCount: 0,
      mergePercentage: 0,
      revertCount: 0,
      medianChurn: 0,
      coAuthorCount: 0,
      coAuthorPercentage: 0,
    };
  }

  let conventionalCount = 0;
  let signedCount = 0;
  let mergeCount = 0;
  let revertCount = 0;
  let coAuthorCount = 0;
  const churns: number[] = [];

  for (const c of commits) {
    if (isConventionalCommit(c.summary)) conventionalCount++;
    if (c.gpg_status === "G" || c.gpg_status === "U") signedCount++;
    if (c.is_merge) mergeCount++;
    if (c.is_revert) revertCount++;
    if (c.co_authors.length > 0) coAuthorCount++;

    // Churn for non-merge commits
    if (!c.is_merge) {
      churns.push(c.additions + c.deletions);
    }
  }

  churns.sort((a, b) => a - b);
  let medianChurn = 0;
  if (churns.length > 0) {
    const mid = Math.floor(churns.length / 2);
    medianChurn =
      churns.length % 2 !== 0 ? churns[mid] : Math.round((churns[mid - 1] + churns[mid]) / 2);
  }

  const total = commits.length;
  const pct = (n: number) => Math.round((n / total) * 100);

  return {
    totalCommits: total,
    conventionalCount,
    conventionalPercentage: pct(conventionalCount),
    signedCount,
    signedPercentage: pct(signedCount),
    mergeCount,
    mergePercentage: pct(mergeCount),
    revertCount,
    medianChurn,
    coAuthorCount,
    coAuthorPercentage: pct(coAuthorCount),
  };
}

/**
 * Computes hotspot risk items by cross-referencing file churn against test coverage.
 */
export function computeHotspotRisks(
  topFiles: readonly PulseFileChurn[],
  coverageReport: CoverageReport | null,
): readonly HotspotRiskItem[] {
  if (!topFiles || topFiles.length === 0) return [];

  const coverageLoaded = coverageReport !== null && Array.isArray(coverageReport.files);

  // Build lookup map for coverage report files
  const covMap = new Map<string, { percentage: number; linesFound: number; linesHit: number }>();
  if (coverageLoaded) {
    for (const f of coverageReport.files) {
      covMap.set(f.path, {
        percentage: f.percentage,
        linesFound: f.lines_found,
        linesHit: f.lines_hit,
      });
    }
  }

  const items: HotspotRiskItem[] = [];

  for (const file of topFiles) {
    const churn = file.additions + file.deletions;
    const cov = covMap.get(file.path);

    let coveragePercentage: number | null = null;
    let linesFound: number | null = null;
    let uncoveredLines: number | null = null;
    let coverageStatus: HotspotRiskItem["coverageStatus"] = coverageLoaded
      ? "missing-file"
      : "unscanned";
    let uncoveredFactor = coverageLoaded ? 0.85 : 0;

    if (cov) {
      coveragePercentage = cov.percentage;
      linesFound = cov.linesFound;
      uncoveredLines = Math.max(0, cov.linesFound - cov.linesHit);
      uncoveredFactor = Math.max(0.05, (100 - cov.percentage) / 100);
      coverageStatus = "hit";
    }

    const riskScore = Math.round(churn * (coverageLoaded ? uncoveredFactor : 1));

    let riskLevel: HotspotRiskItem["riskLevel"];
    if (!coverageLoaded) {
      // Churn-only ranking: never call unscanned files "critical untested".
      if (churn >= 200) riskLevel = "high";
      else if (churn >= 100) riskLevel = "medium";
      else riskLevel = "low";
    } else if (churn >= 200 && (coveragePercentage === null || coveragePercentage < 50)) {
      riskLevel = "critical";
    } else if (churn >= 100 && (coveragePercentage === null || coveragePercentage < 75)) {
      riskLevel = "high";
    } else if (churn >= 50 && (coveragePercentage === null || coveragePercentage < 80)) {
      riskLevel = "medium";
    } else {
      riskLevel = "low";
    }

    items.push({
      path: file.path,
      churn,
      additions: file.additions,
      deletions: file.deletions,
      commitsCount: file.commits_count,
      coveragePercentage,
      linesFound,
      uncoveredLines,
      riskScore,
      riskLevel,
      coverageStatus,
    });
  }

  items.sort((a, b) => b.riskScore - a.riskScore);
  return items;
}

/**
 * Computes period comparison deltas between current window (last 30d) and prior window (30-60d).
 */
export function computePeriodCompare(
  commits: readonly PulseCommitSummary[],
  nowMs = Date.now(),
  periodDays = 30,
): PeriodCompareDeltas {
  const periodMs = periodDays * 86400 * 1000;
  const currentStart = nowMs - periodMs;
  const priorStart = nowMs - 2 * periodMs;

  let currentCommits = 0;
  let priorCommits = 0;
  let currentAdds = 0;
  let priorAdds = 0;
  let currentDels = 0;
  let priorDels = 0;

  const currentDays = new Set<string>();
  const priorDays = new Set<string>();

  for (const c of commits) {
    const commitMs = c.timestamp * 1000;
    const dayKey = formatLocalDayKey(commitMs);

    if (commitMs >= currentStart && commitMs <= nowMs) {
      currentCommits++;
      currentAdds += c.additions;
      currentDels += c.deletions;
      currentDays.add(dayKey);
    } else if (commitMs >= priorStart && commitMs < currentStart) {
      priorCommits++;
      priorAdds += c.additions;
      priorDels += c.deletions;
      priorDays.add(dayKey);
    }
  }

  const calcPct = (curr: number, prior: number): number => {
    if (prior === 0) return curr > 0 ? 100 : 0;
    return Math.round(((curr - prior) / prior) * 100);
  };

  return {
    currentCommits,
    priorCommits,
    commitsDeltaPct: calcPct(currentCommits, priorCommits),
    currentAdds,
    priorAdds,
    addsDeltaPct: calcPct(currentAdds, priorAdds),
    currentDels,
    priorDels,
    delsDeltaPct: calcPct(currentDels, priorDels),
    currentActiveDays: currentDays.size,
    priorActiveDays: priorDays.size,
    activeDaysDelta: currentDays.size - priorDays.size,
  };
}


/**
 * Describes the commit population a summary covers: how many commits, on how
 * many distinct local days, between which two days.
 *
 * Unlike `computeRhythm`, this is bounded by the commits handed to it rather
 * than by a trailing window, because an export card describes the scan it was
 * built from and nothing else. Callers must take the commit count from here
 * too: pairing this active-day count with a differently-filtered total would
 * silently describe two different populations as one.
 */
export function computeCommitWindow(commits: readonly PulseCommitSummary[]): CommitWindow {
  const days = new Set<number>();
  let earliest: number | null = null;
  let latest: number | null = null;

  for (const commit of commits ?? []) {
    if (!Number.isFinite(commit.timestamp) || commit.timestamp <= 0) continue;
    const ms = commit.timestamp * 1000;
    days.add(startOfLocalDay(ms));
    if (earliest === null || ms < earliest) earliest = ms;
    if (latest === null || ms > latest) latest = ms;
  }

  return {
    commits: commits?.length ?? 0,
    activeDays: days.size,
    firstDay: earliest === null ? null : formatLocalDayKey(earliest),
    lastDay: latest === null ? null : formatLocalDayKey(latest),
  };
}
