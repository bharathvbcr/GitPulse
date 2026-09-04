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
}

/* ── View model ──────────────────────────────────────────────────────────── */

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
  | { readonly kind: "read"; readonly value: T; readonly at: number | null; readonly partial: boolean }
  | { readonly kind: "unscanned" }
  | { readonly kind: "failed"; readonly reason: string };

export function readCell<T>(value: T, at: number | null, partial = false): Cell<T> {
  return { kind: "read", value, at, partial };
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

  /* Tier 2 — explicit scans. */
  readonly loc: Cell<LocCellValue>;
  readonly storage: Cell<StorageCellValue>;
  readonly health: Cell<HealthCellValue>;
  readonly coverage: Cell<number>;
}
