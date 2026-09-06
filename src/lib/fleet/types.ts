/**
 * Fleet wire shapes and view model.
 *
 * The wire half mirrors `src-tauri/src/insights/mod.rs` field-for-field and is
 * pinned by `scripts/check-coverage-types.mjs`; the view half is what the grid
 * renders. Both live in a module rather than in the component, because a
 * payload type declared inside a `.svelte` file is unreachable by `check:types`
 * (see scripts/wire-type-locality-contract.test.ts).
 *
 * The one idea running through every type here: **a metric that was never
 * measured, a metric whose measurement failed, and a metric that measured zero
 * are three different facts.** On a grid of two dozen rows the difference is
 * the whole product — collapse it and the dashboard cheerfully reports a fleet
 * of clean, empty, vulnerability-free repositories that nobody ever scanned.
 */

import type { AgentSummary } from "../insights/types";

/* ── Wire: cmd_fleet_snapshot ────────────────────────────────────────────── */

/**
 * Cached metric values read back from one repository's own ledger.
 *
 * Every family carries its own nullable value AND its own timestamp. `null`
 * plus a null timestamp is "never scanned"; a value with a timestamp is a
 * measurement with an age. There is no encoding for "scanned, found nothing,
 * timestamp unknown" because that state does not exist.
 */
export interface FleetMetrics {
  repo_path: string;
  loc: number | null;
  loc_language: string | null;
  /** True when a budget cut the language scan short: the count is a floor. */
  loc_truncated: boolean;
  /** ISO-8601 UTC of the language scan, or null when it never ran. */
  loc_at: string | null;
  storage_bytes: number | null;
  storage_git_bytes: number | null;
  storage_reclaimable_bytes: number | null;
  /** True when a budget cut the storage walk short: the bytes are a floor. */
  storage_truncated: boolean;
  storage_at: string | null;
  vulns_critical: number | null;
  vulns_high: number | null;
  vulns_moderate: number | null;
  vulns_low: number | null;
  vulns_unknown: number | null;
  vulns_total: number | null;
  /** True only when every discovered audit target completed. */
  health_complete: boolean;
  health_at: string | null;
  coverage_pct: number | null;
  coverage_truncated: boolean;
  coverage_at: string | null;
  /**
   * The language breakdown recorded by the same scan that set `loc`.
   *
   * Empty means no breakdown is on file — never "this repository has no
   * languages". `loc_at` is what tells a repository nobody has scanned apart
   * from one that was scanned and genuinely had nothing to count.
   */
  languages: FleetLanguageStat[];

  /* ── The previous measurement, so a number can carry a direction ───────── */
  //
  // Same shape as every family above: a value and the day it was taken,
  // together or not at all. `null` means there is no earlier measurement on
  // file — a first scan, or a ledger predating the history table — and the UI
  // must render NO delta rather than a delta of zero. "Unchanged" the first
  // time something is measured is a claim about a past nobody observed.
  //
  // The baseline is the previous distinct *day* that family was measured, not
  // "a week ago": nobody guarantees a scan happened a week ago, and
  // interpolating one would invent a measurement. The day travels so a delta
  // can name its own baseline.
  loc_prev: number | null;
  loc_prev_day: string | null;
  storage_prev_bytes: number | null;
  storage_prev_day: string | null;
  vulns_prev_total: number | null;
  vulns_prev_day: string | null;
  coverage_prev_pct: number | null;
  coverage_prev_day: string | null;
}

/**
 * One language's share of a repository, as the ledger cached it.
 *
 * Structurally identical to `RepoLanguageStat` on purpose, so the cached rows
 * fold through the very same `pickLanguageBarStats` the per-repository
 * language bar uses. `languageStatContract` in `./languages.ts` holds the two
 * shapes together at compile time; a second, drifting idea of what a language
 * reading is would be worse than the duplication it saves.
 */
export interface FleetLanguageStat {
  language: string;
  color_hex: string;
  category: string;
  code_lines: number;
  file_count: number;
  percentage: number;
}

/**
 * One repository's commit rhythm over a bounded, stated window.
 *
 * Cheap by construction: one `git log` that reads commit metadata and never
 * opens a diff, which is what lets it ride along with the Tier 1 sweep rather
 * than being a scan somebody has to ask for.
 */
export interface FleetCommitStats {
  /** Days the window spans, and the length of `daily`. */
  window_days: number;
  /**
   * Unix seconds the window ends at. Every repository in one sweep shares
   * this, which is what makes the series summable bucket for bucket.
   */
  anchor_epoch: number;
  /** Commits in the window. Always equals the sum of `daily`. */
  commits: number;
  authors: number;
  /** Buckets carrying at least one commit. */
  active_days: number;
  commits_7d: number;
  commits_prior_7d: number;
  /** Newest commit in the window; 0 when the window is empty. */
  last_commit_epoch: number;
  /** One count per bucket, oldest first, exactly `window_days` long. */
  daily: number[];
  /** True when the commit cap stopped the walk: every count above is a floor. */
  truncated: boolean;
}

/** One repository's cheap live facet. Failure is per repository, never global. */
export interface FleetRepoFacet {
  repo_path: string;
  /** False when the repository could not be opened at all. */
  ok: boolean;
  error: string;
  /** True when the worktree listing ran; false leaves the counts meaningless. */
  worktrees_ok: boolean;
  worktrees_error: string;
  worktrees: number;
  agents: AgentSummary;
  /** True when the last-commit probe ran. */
  last_commit_ok: boolean;
  /** Unix seconds of the newest commit on HEAD; 0 when unread. */
  last_commit_epoch: number;
  /** True when the commit-rhythm probe ran. False leaves `commits` null. */
  commits_ok: boolean;
  commits_error: string;
  /** Non-null exactly when `commits_ok`. An empty window inside it is a
   *  measured silence; null is not. */
  commits: FleetCommitStats | null;
  /** True when this repository's ledger could be consulted at all. */
  metrics_ok: boolean;
  metrics_error: string;
  /** Persisted metric cache for this repository, or null when it has none. */
  metrics: FleetMetrics | null;
}

export interface FleetSnapshot {
  repos: FleetRepoFacet[];
  requested: number;
  scanned: number;
  /** Unix seconds every commit window in this sweep is anchored at. */
  anchor_epoch: number;
  /** True when a cap or the sweep deadline stopped the walk short. */
  truncated: boolean;
  duration_ms: number;
}

/* ── Wire: cmd_fleet_record_metrics ──────────────────────────────────────── */

/**
 * One family's worth of freshly scanned numbers, written back to the repo's
 * own ledger. Exactly one field group is populated per call; the rest stay
 * null and leave whatever was already recorded untouched.
 */
export interface FleetMetricsInput {
  loc: number | null;
  loc_language: string | null;
  loc_truncated: boolean;
  storage_bytes: number | null;
  storage_git_bytes: number | null;
  storage_reclaimable_bytes: number | null;
  storage_truncated: boolean;
  vulns_critical: number | null;
  vulns_high: number | null;
  vulns_moderate: number | null;
  vulns_low: number | null;
  vulns_unknown: number | null;
  vulns_total: number | null;
  health_complete: boolean;
  coverage_pct: number | null;
  coverage_truncated: boolean;
  /**
   * The breakdown this call recorded, if it is a language scan.
   *
   * Three states on purpose: `null` is "not a language scan" and leaves the
   * stored breakdown alone, `[]` is "a scan ran and found nothing" and clears
   * it, and a list replaces it. Collapsing the first two would make every
   * storage scan quietly erase the last language scan.
   */
  languages: FleetLanguageStat[] | null;
}

/* ── View model ──────────────────────────────────────────────────────────── */

/**
 * The commit windows Fleet Pulse offers.
 *
 * Bounded here as well as in Rust, which clamps anything it is sent: a control
 * that can only emit these three values is a narrower promise than a number
 * field, and the backend's clamp is the guarantee rather than the UI's.
 */
export const COMMIT_WINDOWS: readonly number[] = [30, 90, 180];

/** The window a sweep uses when nobody has chosen one. Mirrors Rust's default. */
export const DEFAULT_COMMIT_WINDOW = 90;

/** The metric families a repository can be scanned for, on demand. */
export type ScanFamily = "loc" | "storage" | "health" | "coverage";

export const SCAN_FAMILIES: readonly ScanFamily[] = ["loc", "storage", "health", "coverage"];

export const FAMILY_LABEL: Readonly<Record<ScanFamily, string>> = {
  loc: "Lines of code",
  storage: "Storage",
  health: "Dependency health",
  coverage: "Coverage",
};

/**
 * How wide each family's fleet sweep may fan out.
 *
 * Storage walks up to 250,000 files behind a 20-second deadline and health
 * spawns `npm audit` / `cargo audit` with a 90-second timeout — running four
 * of either at once is a share of the machine this view has no claim to. The
 * cheap families use the ordinary IPC fan-out width.
 */
export const FAMILY_CONCURRENCY: Readonly<Record<ScanFamily, number>> = {
  loc: 4,
  coverage: 2,
  storage: 2,
  health: 2,
};

/**
 * One measurable cell.
 *
 * `unscanned` and `failed` both render as absences, but they are not the same
 * absence and the reader is told which: nobody has asked yet, versus we asked
 * and could not find out.
 */
export type Cell<T> =
  | {
      readonly kind: "read";
      readonly value: T;
      readonly at: number | null;
      readonly partial: boolean;
      /** How this reading moved since the last day it was taken, if ever. */
      readonly delta?: CellDelta;
    }
  | { readonly kind: "unscanned" }
  | { readonly kind: "failed"; readonly reason: string };

/**
 * How a measurement has moved since the last day it was taken.
 *
 * Only ever constructed when there IS an earlier measurement; the absence of a
 * delta is the absence of this object, never a zero.
 */
export interface CellDelta {
  /** Signed change from the baseline, in the cell's own units. */
  readonly change: number;
  /** The baseline value, so a reader can see what it moved from. */
  readonly from: number;
  /** The day the baseline was measured, `YYYY-MM-DD`. */
  readonly day: string;
}

export function readCell<T>(
  value: T,
  at: number | null,
  partial = false,
  delta?: CellDelta,
): Cell<T> {
  return { kind: "read", value, at, partial, delta };
}

/**
 * A delta, or nothing at all.
 *
 * Returns `undefined` — not a zero-valued delta — whenever there is no
 * baseline to compare against, or either side is non-finite. That is the
 * distinction the whole history table exists to preserve: a first scan has no
 * direction, and rendering one as "unchanged" would be a claim about a past
 * that was never measured.
 */
export function deltaFrom(
  current: number,
  previous: number | null | undefined,
  day: string | null | undefined,
): CellDelta | undefined {
  if (previous === null || previous === undefined) return undefined;
  if (day === null || day === undefined || day === "") return undefined;
  if (!Number.isFinite(current) || !Number.isFinite(previous)) return undefined;
  return { change: current - previous, from: previous, day };
}

export const UNSCANNED: Cell<never> = { kind: "unscanned" };

export function failedCell(reason: string): Cell<never> {
  return { kind: "failed", reason: reason.trim() || "the scan failed for an unstated reason" };
}

/** Storage numbers as the grid reads them. */
export interface StorageCellValue {
  bytes: number;
  gitBytes: number;
  reclaimableBytes: number;
}

/** Vulnerability counts as the grid reads them. */
export interface HealthCellValue {
  critical: number;
  high: number;
  moderate: number;
  low: number;
  unknown: number;
  total: number;
  complete: boolean;
}

export interface LocCellValue {
  lines: number;
  language: string | null;
  /**
   * The cached per-language breakdown, largest first, or empty when none is
   * on file. An empty list next to a non-zero `lines` is a ledger written
   * before breakdowns were recorded — the total stands, the mix is simply not
   * known, and the row must not draw a bar implying otherwise.
   */
  languages: readonly FleetLanguageStat[];
}

/** Commit rhythm as the grid reads it. */
export interface CommitsCellValue {
  /** Days the window spans. Every count here is meaningless without it. */
  windowDays: number;
  /** Commits in the whole window. */
  commits: number;
  authors: number;
  activeDays: number;
  /** Commits in the newest seven buckets, and the seven before them. */
  recent: number;
  prior: number;
  /** Per-bucket counts, oldest first, exactly `windowDays` long. */
  daily: readonly number[];
}

/** Why a row is worth looking at, worst first. Mirrors wipSummary's ordering. */
export type FleetSeverity =
  | "conflicts"
  | "operation"
  | "unknown"
  | "uncommitted"
  | "unpushed"
  | "stash"
  | "clean";

/** Whether this row has a live session behind it. */
export type FleetPresence = "open" | "recent";

export interface FleetRow {
  readonly path: string;
  readonly label: string;
  readonly presence: FleetPresence;
  /** Null for a recents row, and for an open repo that has not hydrated. */
  readonly branch: string | null;
  readonly severity: FleetSeverity;
  /** One short clause naming the worst thing about this repository. */
  readonly headline: string;

  /* Tier 0 — free, from the live session. Absent entirely on recents rows. */
  readonly changes: Cell<{ files: number; staged: number; conflicted: number; additions: number; deletions: number }>;
  readonly sync: Cell<{ ahead: number; behind: number; stash: number }>;
  /** Present when the filesystem watch is not confirmed live. */
  readonly watchWarning: string | null;

  /* Tier 1 — one cheap sweep. */
  readonly work: Cell<{ worktrees: number; agentSessions: number; agentKinds: string[] }>;
  readonly activity: Cell<number>;
  /** Commit rhythm over the sweep's window. Rides along with Tier 1. */
  readonly commits: Cell<CommitsCellValue>;

  /* Tier 2 — explicit scans. */
  readonly loc: Cell<LocCellValue>;
  readonly storage: Cell<StorageCellValue>;
  readonly health: Cell<HealthCellValue>;
  readonly coverage: Cell<number>;
}
