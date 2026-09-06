/**
 * Fleet Pulse: the workspace's commit rhythm, as one readable answer.
 *
 * The per-repository Pulse view answers "how is this repository moving?" from
 * a full churn-and-authorship report. This answers the same question about the
 * whole workspace from the cheap window every Fleet sweep already reads, and
 * it is deliberately a different, smaller question — not a fleet-wide rerun of
 * the expensive one.
 *
 * Two rules shape everything below, and they are the same two the rest of this
 * directory runs on:
 *
 * 1. **A series is only summable when its buckets line up.** Every repository
 *    in one sweep is anchored at the same instant by the backend, so bucket 40
 *    means the same span on every row. A row whose window disagrees is left
 *    out and counted as excluded rather than added in at the wrong offset,
 *    because a chart that is quietly one day out is worse than one that says
 *    it dropped a row.
 * 2. **A total that could not cover everything says so.** `counted` and
 *    `eligible` travel with every number here, exactly as they do on
 *    {@link import("./aggregate").FleetTally}, so nothing on screen can imply
 *    a reading of the whole workspace that was taken from part of it.
 */

import type { FleetRow } from "./types";
import { tallyScope } from "./aggregate";
import { mergeLanguageStats } from "./languages";
import type { FleetLanguageStat } from "./types";
import { plural } from "../format";

/** Buckets one short-term trend covers. Mirrors the backend's `TREND_BUCKETS`. */
export const TREND_DAYS = 7;

/**
 * Which way the fleet is moving, and by how much.
 *
 * `null` deltas are the point: with nothing in the prior period there is no
 * percentage to state, and rendering "+100%" for a fleet that went from zero
 * to one commit is a number that reads as a finding.
 */
export interface PulseTrend {
  readonly recent: number;
  readonly prior: number;
  /** Signed percentage change, or null when the prior period was empty. */
  readonly deltaPct: number | null;
  readonly direction: "up" | "down" | "flat" | "new";
}

export function trendOf(recent: number, prior: number): PulseTrend {
  if (prior <= 0) {
    return {
      recent,
      prior,
      deltaPct: null,
      // "new" and "flat" are different facts: one is a fleet that started
      // moving, the other is one that never did.
      direction: recent > 0 ? "new" : "flat",
    };
  }
  const deltaPct = ((recent - prior) / prior) * 100;
  return {
    recent,
    prior,
    deltaPct,
    direction: recent === prior ? "flat" : recent > prior ? "up" : "down",
  };
}

/** One repository's contribution, for the callouts. */
export interface PulseContributor {
  readonly path: string;
  readonly label: string;
  readonly commits: number;
  readonly recent: number;
}

export interface FleetPulse {
  /** Days the window spans, taken from the rows themselves. */
  readonly windowDays: number;
  /** Summed per-bucket commit counts, oldest first. Empty when nothing counted. */
  readonly daily: readonly number[];
  /** Tallest bucket, for scaling a chart without a second pass. */
  readonly peak: number;
  readonly commits: number;
  /** Buckets in which *any* repository committed. */
  readonly activeDays: number;
  /**
   * Authors, summed across repositories.
   *
   * A person working in three repositories counts three times: the sweep
   * reports a per-repository count, never the identities, so there is nothing
   * here to deduplicate with. The field is named for what it is and the UI
   * labels it "author slots" rather than "people".
   */
  readonly authorSlots: number;
  readonly trend: PulseTrend;
  /** Open repositories in scope. */
  readonly eligible: number;
  /** Of those, how many contributed a readable window. */
  readonly counted: number;
  /** Of the rest, how many failed versus were never swept. */
  readonly failed: number;
  readonly unscanned: number;
  /** Rows whose window did not line up with the majority and were left out. */
  readonly mismatched: number;
  /** True when any counted window was itself capped: every count is a floor. */
  readonly partial: boolean;
  /** Busiest repositories in the trend period, most recent activity first. */
  readonly busiest: readonly PulseContributor[];
  /** Open repositories with a readable window and nothing in it at all. */
  readonly dormant: readonly PulseContributor[];
}

const EMPTY_PULSE: FleetPulse = {
  windowDays: 0,
  daily: [],
  peak: 0,
  commits: 0,
  activeDays: 0,
  authorSlots: 0,
  trend: { recent: 0, prior: 0, deltaPct: null, direction: "flat" },
  eligible: 0,
  counted: 0,
  failed: 0,
  unscanned: 0,
  mismatched: 0,
  partial: false,
  busiest: [],
  dormant: [],
};

/** How many repositories a callout list names before it stops. */
const CALLOUT_LIMIT = 3;

/**
 * The window the majority of readable rows agree on.
 *
 * Rows come from one sweep and so normally agree, but a row can survive from a
 * previous snapshot across a re-render, and two windows of different lengths
 * cannot be summed bucket for bucket at all. Picking the modal window and
 * excluding the rest keeps the chart correct and makes the exclusion countable
 * rather than invisible.
 */
function modalWindow(rows: readonly FleetRow[]): number {
  const counts = new Map<number, number>();
  for (const row of rows) {
    if (row.commits.kind !== "read") continue;
    const days = row.commits.value.windowDays;
    if (!Number.isInteger(days) || days <= 0) continue;
    counts.set(days, (counts.get(days) ?? 0) + 1);
  }
  let best = 0;
  let bestCount = 0;
  for (const [days, count] of counts) {
    // Ties break toward the longer window, so the answer does not flip
    // between renders on an evenly split workspace.
    if (count > bestCount || (count === bestCount && days > best)) {
      best = days;
      bestCount = count;
    }
  }
  return best;
}

function sumTail(daily: readonly number[], from: number, to: number): number {
  let total = 0;
  for (let i = Math.max(0, from); i < Math.min(daily.length, to); i += 1) {
    const value = daily[i];
    if (Number.isFinite(value)) total += value;
  }
  return total;
}

/**
 * Rolls the fleet's commit windows into one.
 *
 * Scope is the open repositories, for the same reason every other fleet total
 * uses `tallyScope`: a recents row has no live session and folding its cached
 * numbers in would mix a measurement of the workspace with a measurement of a
 * history list.
 */
export function fleetPulse(rows: readonly FleetRow[]): FleetPulse {
  const scope = tallyScope(rows);
  if (scope.length === 0) return EMPTY_PULSE;

  const windowDays = modalWindow(scope);
  if (windowDays === 0) {
    // Nothing readable at all. Still report the shortfall, so the panel can
    // say why it is empty instead of drawing a flat line that looks like calm.
    let failed = 0;
    let unscanned = 0;
    for (const row of scope) {
      if (row.commits.kind === "failed") failed += 1;
      else if (row.commits.kind === "unscanned") unscanned += 1;
    }
    return { ...EMPTY_PULSE, eligible: scope.length, failed, unscanned };
  }

  const daily = new Array<number>(windowDays).fill(0);
  const busiest: PulseContributor[] = [];
  const dormant: PulseContributor[] = [];
  let commits = 0;
  let authorSlots = 0;
  let counted = 0;
  let failed = 0;
  let unscanned = 0;
  let mismatched = 0;
  let partial = false;

  for (const row of scope) {
    const cell = row.commits;
    if (cell.kind === "failed") {
      failed += 1;
      continue;
    }
    if (cell.kind === "unscanned") {
      unscanned += 1;
      continue;
    }
    const value = cell.value;
    // A window of a different length, or a series that does not match its own
    // declared length, cannot be added at any offset without being wrong.
    if (value.windowDays !== windowDays || value.daily.length !== windowDays) {
      mismatched += 1;
      continue;
    }
    counted += 1;
    if (cell.partial) partial = true;
    commits += value.commits;
    authorSlots += value.authors;
    for (let i = 0; i < windowDays; i += 1) {
      const count = value.daily[i];
      if (Number.isFinite(count)) daily[i] += count;
    }
    const contributor: PulseContributor = {
      path: row.path,
      label: row.label,
      commits: value.commits,
      recent: value.recent,
    };
    if (value.commits === 0) dormant.push(contributor);
    else busiest.push(contributor);
  }

  busiest.sort(
    (a, b) => b.recent - a.recent || b.commits - a.commits || a.label.localeCompare(b.label),
  );
  dormant.sort((a, b) => a.label.localeCompare(b.label));

  const recent = sumTail(daily, windowDays - TREND_DAYS, windowDays);
  const prior = sumTail(daily, windowDays - TREND_DAYS * 2, windowDays - TREND_DAYS);

  return {
    windowDays,
    daily,
    peak: daily.reduce((max, count) => (count > max ? count : max), 0),
    commits,
    activeDays: daily.reduce((n, count) => (count > 0 ? n + 1 : n), 0),
    authorSlots,
    trend: trendOf(recent, prior),
    eligible: scope.length,
    counted,
    failed,
    unscanned,
    mismatched,
    partial,
    busiest: busiest.slice(0, CALLOUT_LIMIT),
    dormant: dormant.slice(0, CALLOUT_LIMIT),
  };
}

/**
 * The coverage clause for a pulse, or "" when it really did cover everything.
 *
 * Same contract as `describeTally`: an empty string means the reader may take
 * the numbers at face value, and anything else must be rendered next to them.
 */
export function describePulseCoverage(pulse: FleetPulse): string {
  if (pulse.eligible === 0) return "no repositories are open";
  if (pulse.counted === 0) {
    if (pulse.failed > 0) {
      return `no commit history could be read — ${plural(pulse.failed, "repository", "repositories")} failed`;
    }
    return "the sweep has not read commit history yet";
  }
  const parts: string[] = [];
  if (pulse.failed > 0) parts.push(`${pulse.failed} could not be read`);
  if (pulse.unscanned > 0) parts.push(`${pulse.unscanned} not swept`);
  if (pulse.mismatched > 0) parts.push(`${pulse.mismatched} on a different window`);
  const floor = pulse.partial ? ", some histories capped" : "";
  if (parts.length === 0) {
    return floor ? `across all ${pulse.eligible}${floor}` : "";
  }
  return `counted across ${pulse.counted} of ${pulse.eligible} — ${parts.join(", ")}${floor}`;
}

/** The fleet's language mix, and how much of the fleet it was drawn from. */
export interface FleetLanguageMix {
  readonly stats: readonly FleetLanguageStat[];
  readonly totalLines: number;
  /** Repositories that contributed a breakdown. */
  readonly counted: number;
  readonly eligible: number;
  /** Repositories whose language scan is on file but carries no breakdown. */
  readonly withoutBreakdown: number;
  /** True when any contributing scan was itself capped. */
  readonly partial: boolean;
}

/**
 * Merges every open repository's cached breakdown into one fleet mix.
 *
 * A repository that has never been scanned contributes nothing and is counted
 * as such; so is one scanned by a build that recorded totals but no breakdown.
 * The distinction matters because the second one has a lines figure on the
 * grid, and a reader comparing the two would otherwise conclude the mix is
 * missing rather than that it was never recorded.
 */
export function fleetLanguageMix(rows: readonly FleetRow[]): FleetLanguageMix {
  const scope = tallyScope(rows);
  const perRepo: FleetLanguageStat[][] = [];
  let withoutBreakdown = 0;
  let partial = false;
  for (const row of scope) {
    if (row.loc.kind !== "read") continue;
    const languages = row.loc.value.languages;
    if (languages.length === 0) {
      withoutBreakdown += 1;
      continue;
    }
    if (row.loc.partial) partial = true;
    perRepo.push([...languages]);
  }
  const merged = mergeLanguageStats(perRepo);
  return {
    stats: merged.stats,
    totalLines: merged.totalLines,
    counted: merged.counted,
    eligible: scope.length,
    withoutBreakdown,
    partial,
  };
}
