/**
 * Ordering and filtering the Fleet grid.
 *
 * The whole difficulty is one thing: **an unscanned cell has no value to sort
 * by, and a failed one has no value either.** The obvious implementation reads
 * `cell.value ?? 0` and sorts on that, which quietly files every repository
 * nobody has audited alongside the ones that were audited and came back clean
 * — the single failure this directory exists to prevent, reintroduced through
 * a comparator.
 *
 * So absences never take part in the comparison. They sink to the bottom in
 * both directions, keeping their own order relative to each other: measured
 * values first, then never-scanned, then failed. Reversing the sort reverses
 * the measured values and leaves the absences where they are, because "least
 * lines of code" does not mean "we did not look".
 */

import type { Cell, FleetRow } from "./types";
import { severityRank } from "./aggregate";

/** The columns a reader can order the grid by. */
export type SortKey =
  | "repository"
  | "severity"
  | "changes"
  | "sync"
  | "work"
  | "activity"
  | "commits"
  | "loc"
  | "storage"
  | "health"
  | "coverage";

export type SortDirection = "asc" | "desc";

export interface SortState {
  readonly key: SortKey;
  readonly direction: SortDirection;
}

/** The order the grid opens in: worst first, the way `byUrgency` ranks. */
export const DEFAULT_SORT: SortState = { key: "severity", direction: "asc" };

/**
 * Which direction a column should take when it is first clicked.
 *
 * A reader clicking "Vulnerabilities" wants the worst repositories, not the
 * cleanest, and one clicking "Repository" wants A first. Getting this wrong
 * costs a second click every single time, so it is stated per column rather
 * than defaulting to ascending everywhere.
 */
const FIRST_DIRECTION: Readonly<Record<SortKey, SortDirection>> = {
  repository: "asc",
  severity: "asc",
  changes: "desc",
  sync: "desc",
  work: "desc",
  activity: "desc",
  commits: "desc",
  loc: "desc",
  storage: "desc",
  health: "desc",
  coverage: "asc",
};

/**
 * Columns a reader may hide, and what each is called in the menu.
 *
 * Repository is deliberately absent: a grid of measurements with no way to
 * tell which repository they belong to is not a smaller grid, it is a broken
 * one. Severity is absent for the same reason — it is the stripe on every row,
 * not a column of its own.
 */
export const HIDEABLE_COLUMNS: readonly SortKey[] = [
  "changes",
  "sync",
  "work",
  "commits",
  "activity",
  "loc",
  "storage",
  "health",
  "coverage",
];

/**
 * Which columns are measurable, so hiding one can be checked for hidden
 * failures. The two derived columns have no cell behind them.
 */
export const MEASURED_COLUMNS: readonly SortKey[] = HIDEABLE_COLUMNS;

/** The next sort state after clicking `key`, given the current one. */
export function nextSort(current: SortState, key: SortKey): SortState {
  if (current.key !== key) return { key, direction: FIRST_DIRECTION[key] };
  return { key, direction: current.direction === "asc" ? "desc" : "asc" };
}

/**
 * How a cell ranks against another when neither is being compared by value.
 *
 * Read beats unscanned beats failed. The order is not arbitrary: "we have not
 * looked" is a smaller claim than "we looked and could not find out", and the
 * reader scanning down a column should hit the second group last, where it is
 * a list of things to fix.
 */
function absenceRank(cell: Cell<unknown>): number {
  if (cell.kind === "read") return 0;
  if (cell.kind === "unscanned") return 1;
  return 2;
}

/**
 * Compares two cells of the same column.
 *
 * Returns `null` when both are readable and the caller must compare the values
 * itself; any other result already encodes the absence ordering and must be
 * used as-is, unreversed.
 */
function compareAbsence(a: Cell<unknown>, b: Cell<unknown>): number | null {
  const ra = absenceRank(a);
  const rb = absenceRank(b);
  if (ra === 0 && rb === 0) return null;
  return ra - rb;
}

/** The number a readable cell sorts by, per column. */
function amountOf(row: FleetRow, key: SortKey): number {
  switch (key) {
    case "changes":
      return row.changes.kind === "read" ? row.changes.value.files : 0;
    case "sync":
      // One number for a two-number column: what is unpushed plus what is
      // behind, which is what "how far out of sync is this" actually means.
      return row.sync.kind === "read" ? row.sync.value.ahead + row.sync.value.behind : 0;
    case "work":
      return row.work.kind === "read"
        ? row.work.value.worktrees + row.work.value.agentSessions
        : 0;
    case "activity":
      return row.activity.kind === "read" ? row.activity.value : 0;
    case "commits":
      return row.commits.kind === "read" ? row.commits.value.commits : 0;
    case "loc":
      return row.loc.kind === "read" ? row.loc.value.lines : 0;
    case "storage":
      return row.storage.kind === "read" ? row.storage.value.bytes : 0;
    case "health":
      return row.health.kind === "read" ? row.health.value.total : 0;
    case "coverage":
      return row.coverage.kind === "read" ? row.coverage.value : 0;
    default:
      return 0;
  }
}

/** The cell a column sorts on, or null for the two derived columns. */
export function cellOf(row: FleetRow, key: SortKey): Cell<unknown> | null {
  switch (key) {
    case "changes":
      return row.changes;
    case "sync":
      return row.sync;
    case "work":
      return row.work;
    case "activity":
      return row.activity;
    case "commits":
      return row.commits;
    case "loc":
      return row.loc;
    case "storage":
      return row.storage;
    case "health":
      return row.health;
    case "coverage":
      return row.coverage;
    default:
      return null;
  }
}

/**
 * Orders the grid.
 *
 * Ties always fall through to the label, so the order is total and a row never
 * moves between renders because two repositories happen to have the same
 * number of changed files.
 */
export function sortRows(rows: readonly FleetRow[], sort: SortState): FleetRow[] {
  const sign = sort.direction === "asc" ? 1 : -1;
  return [...rows].sort((a, b) => {
    if (sort.key === "repository") {
      return sign * a.label.localeCompare(b.label);
    }
    if (sort.key === "severity") {
      const bySeverity = severityRank(a.severity) - severityRank(b.severity);
      if (bySeverity !== 0) return sign * bySeverity;
      // Open before recents at equal severity, in both directions: one is the
      // workspace and the other is a history entry, and reversing the sort
      // does not change which is which.
      if (a.presence !== b.presence) return a.presence === "open" ? -1 : 1;
      return a.label.localeCompare(b.label);
    }
    const cellA = cellOf(a, sort.key);
    const cellB = cellOf(b, sort.key);
    if (cellA && cellB) {
      const absence = compareAbsence(cellA, cellB);
      // Deliberately unreversed: an absence sinks in both directions.
      if (absence !== null) return absence || a.label.localeCompare(b.label);
    }
    const delta = amountOf(a, sort.key) - amountOf(b, sort.key);
    if (delta !== 0) return sign * delta;
    return a.label.localeCompare(b.label);
  });
}

/**
 * Filters rows by a free-text query.
 *
 * Matches the label, the full path, the branch and the recorded language — the
 * four things a reader actually types when hunting for a repository in a
 * workspace of two dozen. Plain case-insensitive substring, never a regular
 * expression: an unanchored user-supplied pattern over two dozen rows on every
 * keystroke is exactly the backtracking hazard the explorer's filter box had
 * to be fixed for.
 */
export function searchRows(rows: readonly FleetRow[], query: string): FleetRow[] {
  const needle = query.trim().toLowerCase();
  if (needle === "") return [...rows];
  return rows.filter((row) => {
    if (row.label.toLowerCase().includes(needle)) return true;
    if (row.path.toLowerCase().includes(needle)) return true;
    if (row.branch !== null && row.branch.toLowerCase().includes(needle)) return true;
    if (row.loc.kind === "read") {
      const language = row.loc.value.language;
      if (language !== null && language.toLowerCase().includes(needle)) return true;
      // A repository is findable by any language in its mix, not only the
      // dominant one — searching "rust" should find the mostly-TypeScript app
      // that has a Rust core in it.
      if (row.loc.value.languages.some((s) => s.language.toLowerCase().includes(needle))) {
        return true;
      }
    }
    return false;
  });
}

/**
 * Failures the reader can no longer see, because their column is hidden.
 *
 * A hidden column is still a column that could not be read, and letting a
 * failure disappear along with it would be the same lie as a cell that renders
 * "could not read" as a blank — just moved up a level. So hiding is allowed and
 * the grid reports what hiding cost, by column and by count.
 *
 * Counts open repositories only, matching every other fleet total: a recents
 * row's Tier 2 cells are whatever its ledger happened to hold.
 */
export function hiddenFailures(
  rows: readonly FleetRow[],
  hidden: ReadonlySet<string>,
): { key: SortKey; count: number }[] {
  const out: { key: SortKey; count: number }[] = [];
  for (const key of MEASURED_COLUMNS) {
    if (!hidden.has(key)) continue;
    let count = 0;
    for (const row of rows) {
      if (row.presence !== "open") continue;
      if (cellOf(row, key)?.kind === "failed") count += 1;
    }
    if (count > 0) out.push({ key, count });
  }
  return out;
}
