/**
 * Fleet totals, and the sentence that keeps them honest.
 *
 * A number at the top of a dashboard is read as a fact about everything below
 * it. On this grid that is almost never true: some repositories have never
 * been scanned for a family, some scans failed, and recents rows have no live
 * session at all. So a tally never travels alone — it carries how many
 * repositories it actually counted, and {@link describeTally} refuses to
 * render a bare number whenever that is fewer than all of them.
 *
 * This is the same rule `summarizeRun` enforces for bulk operations
 * ("Fetched 20 of 24 — 1 failed, 3 skipped"), applied to columns instead of
 * runs.
 */

import type { Cell, FleetRow, FleetSeverity } from "./types";

export interface FleetTally {
  /** The reduced value across every repository that contributed. */
  readonly value: number;
  /** Repositories whose cell was read. */
  readonly counted: number;
  /** Repositories in scope for this tally. */
  readonly eligible: number;
  /** Of the uncounted, how many failed versus were never scanned. */
  readonly failed: number;
  readonly unscanned: number;
  /** True when at least one counted cell was itself partial (a floor). */
  readonly partial: boolean;
}

export function isComplete(tally: FleetTally): boolean {
  return tally.eligible > 0 && tally.counted === tally.eligible && !tally.partial;
}

/**
 * Rows a fleet total is computed over: the open repositories.
 *
 * Recents rows show whatever their own ledger recorded, but they have no
 * session, no live state, and no guarantee the path still resolves — folding
 * them into a total would mix a measurement of the workspace with a
 * measurement of a history list.
 */
export function tallyScope(rows: readonly FleetRow[]): FleetRow[] {
  return rows.filter((row) => row.presence === "open");
}

/**
 * Sums one column across the open repositories.
 *
 * `select` picks the cell; `amount` turns a read value into the number being
 * summed. Cells that were never scanned or that failed contribute nothing to
 * `value` and are counted separately, which is what makes the shortfall
 * visible instead of silently depressing the total.
 */
export function tally<T>(
  rows: readonly FleetRow[],
  select: (row: FleetRow) => Cell<T>,
  amount: (value: T) => number,
): FleetTally {
  const scope = tallyScope(rows);
  let value = 0;
  let counted = 0;
  let failed = 0;
  let unscanned = 0;
  let partial = false;
  for (const row of scope) {
    const cell = select(row);
    if (cell.kind === "read") {
      const contribution = amount(cell.value);
      // A non-finite contribution would poison the whole total into NaN, which
      // renders as a broken cell rather than as the shortfall it really is.
      if (Number.isFinite(contribution)) {
        value += contribution;
        counted += 1;
        if (cell.partial) partial = true;
      } else {
        failed += 1;
      }
      continue;
    }
    if (cell.kind === "failed") failed += 1;
    else unscanned += 1;
  }
  return { value, counted, eligible: scope.length, failed, unscanned, partial };
}

function plural(count: number, singular: string, pluralForm = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : pluralForm}`;
}

/**
 * The coverage clause for a tally, or "" when the total really is complete.
 *
 * Never returns something reassuring for an empty scope: with nothing counted
 * there is no total, and the caller renders the clause instead of a number.
 */
export function describeTally(tally: FleetTally): string {
  if (tally.eligible === 0) return "no repositories in scope";
  if (tally.counted === 0) {
    return tally.failed > 0
      ? `not counted — ${plural(tally.failed, "repository", "repositories")} failed to scan`
      : "not scanned";
  }
  const parts: string[] = [];
  if (tally.failed > 0) parts.push(`${tally.failed} failed`);
  if (tally.unscanned > 0) parts.push(`${tally.unscanned} not scanned`);
  if (parts.length === 0) {
    return tally.partial ? `across all ${tally.eligible}, some counts partial` : "";
  }
  const detail = ` — ${parts.join(", ")}`;
  const floor = tally.partial ? ", some counts partial" : "";
  return `counted across ${tally.counted} of ${tally.eligible}${detail}${floor}`;
}

/**
 * Severity order, worst first — the same ranking `wipSummary` uses, with
 * `clean` appended so a fully-known-good repository sorts last rather than
 * being treated as an unrecognized kind.
 */
const SEVERITY_ORDER: readonly FleetSeverity[] = [
  "conflicts",
  "operation",
  "unknown",
  "uncommitted",
  "unpushed",
  "stash",
  "clean",
];

export function severityRank(severity: FleetSeverity): number {
  const index = SEVERITY_ORDER.indexOf(severity);
  // An unrecognized severity from a newer caller sorts last rather than first,
  // so it can never displace a real conflict at the top of the grid.
  return index < 0 ? SEVERITY_ORDER.length : index;
}

/** Rows worth acting on, worst first, then alphabetical inside a band. */
export function byUrgency(rows: readonly FleetRow[]): FleetRow[] {
  return [...rows].sort((a, b) => {
    const bySeverity = severityRank(a.severity) - severityRank(b.severity);
    if (bySeverity !== 0) return bySeverity;
    // Open repositories outrank recents at equal severity: one is the
    // workspace, the other is a history entry.
    if (a.presence !== b.presence) return a.presence === "open" ? -1 : 1;
    return a.label.localeCompare(b.label);
  });
}

export interface FleetHeadline {
  /** Open repositories on the grid. */
  readonly open: number;
  /** Open repositories with something worth acting on. */
  readonly attention: number;
  /** Open repositories whose state could not be determined. */
  readonly unknown: number;
  readonly sentence: string;
}

/**
 * The workspace answer in one sentence, for the header band.
 *
 * Says "nothing needs attention" only when every open repository was examined
 * AND every one came back clean. Anything unknown is named, because "we could
 * not check" and "we checked and it is clean" must not read the same.
 */
export function fleetHeadline(rows: readonly FleetRow[]): FleetHeadline {
  const scope = tallyScope(rows);
  const unknown = scope.filter((row) => row.severity === "unknown").length;
  const attention = scope.filter((row) => row.severity !== "clean").length;
  if (scope.length === 0) {
    return { open: 0, attention: 0, unknown: 0, sentence: "No repositories are open." };
  }
  if (attention === 0) {
    return {
      open: scope.length,
      attention,
      unknown,
      sentence:
        scope.length === 1
          ? "One repository open, and it is clean."
          : `${scope.length} repositories open, all clean.`,
    };
  }
  const worst = byUrgency(scope)[0];
  const unknownClause = unknown > 0 ? `, ${unknown} unreadable` : "";
  return {
    open: scope.length,
    attention,
    unknown,
    sentence:
      `${plural(attention, "repository", "repositories")} of ${scope.length} need attention` +
      `${unknownClause} — ${worst.label}: ${worst.headline}`,
  };
}
