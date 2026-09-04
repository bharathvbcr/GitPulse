/**
 * Building the Fleet grid's rows.
 *
 * Pure: facts in, rows out. No IPC, no store reads, no clock of its own — so
 * every rule below is a unit test rather than something you have to reproduce
 * in the UI to check.
 *
 * Severity and the headline clause are NOT re-derived here. They come from
 * `repoWip`, the same model the workspace close-guard and the tab bar use, so
 * a repository cannot be "at risk" in one surface and fine in another.
 */

import { toWipInput, unknownFacts, type RepoFacts } from "../repos/facts";
import { repoWip, type WipReasonKind } from "../repos/wipSummary";
import { isLiveUpdating } from "../repos/watchState";
import { formatAge } from "../storage/format";
import {
  UNSCANNED,
  failedCell,
  readCell,
  type Cell,
  type FleetMetrics,
  type FleetRepoFacet,
  type FleetRow,
  type FleetSeverity,
  type FleetSnapshot,
  type ScanFamily,
} from "./types";

/** Per-repository, per-family failures recorded during this session's scans. */
export type ScanFailures = Readonly<Record<string, Partial<Record<ScanFamily, string>>>>;

export interface FleetRowInputs {
  /** Live facts for every open tab, in tab order. */
  readonly open: readonly RepoFacts[];
  /** Recent paths with no open tab. Label is already disambiguated. */
  readonly recents: readonly { readonly path: string; readonly label: string }[];
  /** The cheap sweep's result, or null when it has not run. */
  readonly snapshot: FleetSnapshot | null;
  /** A failure of the sweep as a whole, which fails every Tier 1 cell. */
  readonly snapshotError: string | null;
  readonly scanFailures: ScanFailures;
  readonly now: number;
}

/**
 * Milliseconds since the epoch for an ISO-8601 stamp the backend wrote.
 * Returns null for anything unparseable rather than `NaN`, which would render
 * as a plausible-looking age.
 */
export function parseStamp(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  return Number.isFinite(ms) ? ms : null;
}

function severityOf(kind: WipReasonKind | null): FleetSeverity {
  return kind ?? "clean";
}

/**
 * Severity order, worst first — the same ranking `wipSummary` uses, with
 * `clean` appended. Duplicated from `aggregate` deliberately: importing it
 * would make this module depend on the one that consumes it.
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

function rank(severity: FleetSeverity): number {
  const index = SEVERITY_ORDER.indexOf(severity);
  return index < 0 ? SEVERITY_ORDER.length : index;
}

/**
 * What the cheap sweep could not tell us about this repository, if anything.
 *
 * Tier 0 is authoritative for changes and sync, so a failed sweep does not
 * make those numbers wrong. But the row's headline is a claim about the
 * repository as a whole, and calling one "clean" while four of its cells read
 * "could not read" is the same lie in miniature that this codebase refuses
 * everywhere else: a check that could not run must not report what a check
 * that ran and passed would.
 */
function sweepFailure(
  facet: FleetRepoFacet | undefined,
  snapshotError: string | null,
): string | null {
  if (snapshotError) return snapshotError;
  if (!facet) return null;
  if (!facet.ok) return facet.error || "the repository could not be read";
  if (!facet.worktrees_ok) {
    return facet.worktrees_error || "the worktree list could not be read";
  }
  if (!facet.last_commit_ok) return "the last commit could not be read";
  return null;
}

/**
 * Folds an unreadable sweep into the row's verdict — but only downward.
 *
 * `unknown` outranks `uncommitted` and below, and is outranked by conflicts
 * and a parked operation. A failed sweep must never make a repository holding
 * real conflicts look merely unreadable, so the worse of the two wins.
 */
function withSweepFailure(
  severity: FleetSeverity,
  headline: string,
  failure: string | null,
): { severity: FleetSeverity; headline: string } {
  if (failure === null) return { severity, headline };
  if (rank(severity) <= rank("unknown")) return { severity, headline };
  return { severity: "unknown", headline: failure };
}

/**
 * The failure text for a family, with the age of the last good value folded in.
 *
 * A fresh failure always beats a cached number: the user just asked, and a
 * stale value rendered as current is the exact shape this codebase refuses.
 * But the cached value is real, so its age travels in the reason instead of
 * being thrown away.
 */
function scanFailure(reason: string, lastGoodAt: number | null, now: number): Cell<never> {
  if (lastGoodAt === null) return failedCell(reason);
  return failedCell(`${reason} — last successful scan ${formatAge(lastGoodAt, now)}`);
}

function locCell(
  metrics: FleetMetrics | null,
  failure: string | undefined,
  now: number,
): FleetRow["loc"] {
  const at = parseStamp(metrics?.loc_at);
  if (failure) return scanFailure(failure, at, now);
  if (!metrics || metrics.loc === null || at === null) return UNSCANNED;
  return readCell(
    { lines: metrics.loc, language: metrics.loc_language },
    at,
    // A capped language scan counted part of the tree. Rendering its total
    // like a complete one is presenting a sample as full coverage.
    metrics.loc_truncated,
  );
}

function storageCell(
  metrics: FleetMetrics | null,
  failure: string | undefined,
  now: number,
): FleetRow["storage"] {
  const at = parseStamp(metrics?.storage_at);
  if (failure) return scanFailure(failure, at, now);
  if (!metrics || metrics.storage_bytes === null || at === null) return UNSCANNED;
  return readCell(
    {
      bytes: metrics.storage_bytes,
      gitBytes: metrics.storage_git_bytes ?? 0,
      reclaimableBytes: metrics.storage_reclaimable_bytes ?? 0,
    },
    at,
    // A budget-truncated walk reports floors. Rendering them like complete
    // totals is how "we stopped counting" becomes "that is all there is".
    metrics.storage_truncated,
  );
}

function healthCell(
  metrics: FleetMetrics | null,
  failure: string | undefined,
  now: number,
): FleetRow["health"] {
  const at = parseStamp(metrics?.health_at);
  if (failure) return scanFailure(failure, at, now);
  if (!metrics || metrics.vulns_total === null || at === null) return UNSCANNED;
  return readCell(
    {
      critical: metrics.vulns_critical ?? 0,
      high: metrics.vulns_high ?? 0,
      moderate: metrics.vulns_moderate ?? 0,
      low: metrics.vulns_low ?? 0,
      unknown: metrics.vulns_unknown ?? 0,
      total: metrics.vulns_total,
      complete: metrics.health_complete,
    },
    at,
    // An audit where some target never ran found fewer vulnerabilities than
    // exist, and must not read like a clean bill of health.
    !metrics.health_complete,
  );
}

function coverageCell(
  metrics: FleetMetrics | null,
  failure: string | undefined,
  now: number,
): FleetRow["coverage"] {
  const at = parseStamp(metrics?.coverage_at);
  if (failure) return scanFailure(failure, at, now);
  if (!metrics || metrics.coverage_pct === null || at === null) return UNSCANNED;
  return readCell(metrics.coverage_pct, at, metrics.coverage_truncated);
}

/** Tier 1 cells for one repository, given its facet (or the sweep's failure). */
function tierOne(
  facet: FleetRepoFacet | undefined,
  snapshotError: string | null,
): Pick<FleetRow, "work" | "activity"> {
  if (snapshotError) {
    const failure = failedCell(snapshotError);
    return { work: failure, activity: failure };
  }
  if (!facet) return { work: UNSCANNED, activity: UNSCANNED };
  if (!facet.ok) {
    const failure = failedCell(facet.error || "the repository could not be read");
    return { work: failure, activity: failure };
  }
  return {
    work: facet.worktrees_ok
      ? readCell(
          {
            worktrees: facet.worktrees,
            agentSessions: facet.agents.sessions,
            agentKinds: facet.agents.kinds.map((kind) => kind.kind),
          },
          null,
        )
      : failedCell(facet.worktrees_error || "the worktree list could not be read"),
    activity:
      facet.last_commit_ok && facet.last_commit_epoch > 0
        ? readCell(facet.last_commit_epoch * 1000, null)
        : facet.last_commit_ok
          ? // A repository with no commits at all is a real, readable answer.
            readCell(0, null)
          : failedCell("the last commit could not be read"),
  };
}

/**
 * Whether the watch state is worth a marker, and what it says.
 *
 * An unwatched repository shows a stale branch, a stale graph and a stale
 * merge banner indefinitely while presenting as live — so the row says so
 * rather than letting the numbers age silently.
 */
function watchWarningFor(facts: RepoFacts): string | null {
  if (isLiveUpdating(facts.watch)) return null;
  if (facts.watch.status === "unknown") {
    return facts.hydrated ? "Live updates are not confirmed for this repository." : null;
  }
  return facts.watch.reason
    ? `Not receiving live updates: ${facts.watch.reason}`
    : "Not receiving live updates.";
}

/**
 * Why every Tier 2 cell for this repository is unreadable, if it is.
 *
 * A ledger that could not be opened is not a repository that was never
 * scanned. Returning `undefined` there would render a full scan history as
 * four "not scanned" cells — a check that could not run wearing the face of
 * one that was never asked for.
 */
function ledgerFailure(facet: FleetRepoFacet | undefined): string | undefined {
  if (!facet || facet.metrics_ok) return undefined;
  return facet.metrics_error || "this repository's scan history could not be read";
}

function openRow(
  facts: RepoFacts,
  facet: FleetRepoFacet | undefined,
  inputs: FleetRowInputs,
): FleetRow {
  const wip = repoWip(toWipInput(facts));
  const sessionFailures = inputs.scanFailures[facts.path] ?? {};
  const ledger = ledgerFailure(facet);
  // A failure from this session's own scan is fresher than a ledger read
  // failure, so it wins where both are present.
  const failures = {
    loc: sessionFailures.loc ?? ledger,
    storage: sessionFailures.storage ?? ledger,
    health: sessionFailures.health ?? ledger,
    coverage: sessionFailures.coverage ?? ledger,
  };
  const metrics = facet?.metrics ?? null;
  const known = facts.hydrated && !facts.loadFailed;
  const verdict = withSweepFailure(
    severityOf(wip.severity),
    wip.reasons[0]?.detail ?? (facts.isBare ? "bare repository" : "clean"),
    sweepFailure(facet, inputs.snapshotError),
  );
  return {
    path: facts.path,
    label: facts.label,
    presence: "open",
    branch: facts.branch,
    severity: verdict.severity,
    headline: verdict.headline,
    changes: known
      ? readCell(
          {
            files: facts.changedFiles,
            staged: facts.stagedFiles,
            conflicted: facts.conflictedFiles,
            additions: facts.additions,
            deletions: facts.deletions,
          },
          null,
          facts.churnPartial,
        )
      : facts.loadFailed
        ? failedCell(facts.loadError ?? "the repository snapshot could not be read")
        : UNSCANNED,
    sync: known
      ? readCell(
          {
            ahead: facts.unpushedCommits,
            behind: facts.behindCommits,
            stash: facts.stashEntries,
          },
          null,
          // An unreadable stash makes the stash count a floor, not a total.
          facts.stashFailed,
        )
      : facts.loadFailed
        ? failedCell(facts.loadError ?? "the repository snapshot could not be read")
        : UNSCANNED,
    watchWarning: watchWarningFor(facts),
    ...tierOne(facet, inputs.snapshotError),
    loc: locCell(metrics, failures.loc, inputs.now),
    storage: storageCell(metrics, failures.storage, inputs.now),
    health: healthCell(metrics, failures.health, inputs.now),
    coverage: coverageCell(metrics, failures.coverage, inputs.now),
  };
}

/**
 * A repository that is in the recents list but has no open tab.
 *
 * It has no session, so every live fact is unknown — and saying so is the
 * whole point of showing the row at all. Only what its own ledger already
 * recorded can be filled in, and `aggregate` excludes these rows from every
 * fleet total.
 */
function recentRow(
  entry: { path: string; label: string },
  facet: FleetRepoFacet | undefined,
  inputs: FleetRowInputs,
): FleetRow {
  const sessionFailures = inputs.scanFailures[entry.path] ?? {};
  const ledger = ledgerFailure(facet);
  const failures = {
    loc: sessionFailures.loc ?? ledger,
    storage: sessionFailures.storage ?? ledger,
    health: sessionFailures.health ?? ledger,
    coverage: sessionFailures.coverage ?? ledger,
  };
  const metrics = facet?.metrics ?? null;
  return {
    path: entry.path,
    label: entry.label,
    presence: "recent",
    branch: null,
    severity: "unknown",
    headline: "not open — nothing is being watched here",
    changes: UNSCANNED,
    sync: UNSCANNED,
    watchWarning: null,
    work: UNSCANNED,
    activity: UNSCANNED,
    loc: locCell(metrics, failures.loc, inputs.now),
    storage: storageCell(metrics, failures.storage, inputs.now),
    health: healthCell(metrics, failures.health, inputs.now),
    coverage: coverageCell(metrics, failures.coverage, inputs.now),
  };
}

/**
 * Builds every row, open repositories first, then recents.
 *
 * Order within each group is the order it was given — tab order for open
 * repositories, recency for the rest — so the grid is stable between renders
 * and a repository does not jump because a scan landed.
 */
export function buildFleetRows(inputs: FleetRowInputs): FleetRow[] {
  const facets = new Map<string, FleetRepoFacet>();
  for (const facet of inputs.snapshot?.repos ?? []) {
    facets.set(facet.repo_path, facet);
  }
  const openPaths = new Set(inputs.open.map((facts) => facts.path));
  const rows = inputs.open.map((facts) => openRow(facts, facets.get(facts.path), inputs));
  for (const entry of inputs.recents) {
    // A path that is also an open tab is one repository, not two rows.
    if (openPaths.has(entry.path)) continue;
    rows.push(recentRow(entry, facets.get(entry.path), inputs));
  }
  return rows;
}

/** A blank row for a path with no facts at all; used by tests and empty states. */
export function placeholderRow(path: string, label: string, now: number): FleetRow {
  return openRow(unknownFacts(path, label), undefined, {
    open: [],
    recents: [],
    snapshot: null,
    snapshotError: null,
    scanFailures: {},
    now,
  });
}
