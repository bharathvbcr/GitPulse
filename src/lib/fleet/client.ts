/**
 * The Fleet grid's IPC seam, and the mapping from each view's own scan report
 * into the flat metric row the grid caches.
 *
 * No new backend work happens here for Tier 2: a Fleet storage scan runs
 * exactly the command the Storage view runs, and a Fleet audit runs exactly
 * the command the Health view runs. One canonical scanner per family, called
 * from two places, rather than a second implementation that can drift into
 * disagreeing with the view it summarizes.
 *
 * Command names are spelled out literally at each call rather than selected
 * into a variable: `check:ipc` verifies every invoked command against the Rust
 * registry statically, and a computed name is a hole in that check. The
 * injected seam is named `invokeFn` for the same reason — that is one of the
 * two callee names the checker recognises, so a rename would make these call
 * sites invisible to it.
 */

import { invoke } from "@tauri-apps/api/core";
import type { LanguageStatsReport } from "../language/barStats";
import type { StorageReport } from "../storage/types";
import type { DepsHealthReport } from "../health/types";
import type { CoverageReport } from "../coverage/types";
import type { FleetMetricsInput, FleetSnapshot, ScanFamily } from "./types";

export type InvokeFn = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/** Every field null: the shape a family-specific mapper fills in one group of. */
export function emptyMetricsInput(): FleetMetricsInput {
  return {
    loc: null,
    loc_language: null,
    loc_truncated: false,
    storage_bytes: null,
    storage_git_bytes: null,
    storage_reclaimable_bytes: null,
    storage_truncated: false,
    vulns_critical: null,
    vulns_high: null,
    vulns_moderate: null,
    vulns_low: null,
    vulns_unknown: null,
    vulns_total: null,
    health_complete: false,
    coverage_pct: null,
    coverage_truncated: false,
  };
}

export function fetchFleetSnapshot(
  repoPaths: readonly string[],
  invokeFn: InvokeFn = invoke,
): Promise<FleetSnapshot> {
  return invokeFn<FleetSnapshot>("cmd_fleet_snapshot", { repoPaths: [...repoPaths] });
}

export function recordFleetMetrics(
  repoPath: string,
  metrics: FleetMetricsInput,
  invokeFn: InvokeFn = invoke,
): Promise<void> {
  return invokeFn<void>("cmd_fleet_record_metrics", { repoPath, metrics });
}

/**
 * Total lines and the dominant language.
 *
 * Summed across every reported language rather than programming ones alone:
 * the column says "lines of code in this repository", and silently dropping
 * the data and markup would make two repositories with identical trees report
 * different sizes depending on what they are written in. The headline language
 * is the largest by the backend's own percentage.
 */
export function metricsFromLanguages(report: LanguageStatsReport): FleetMetricsInput {
  const stats = report.stats ?? [];
  let lines = 0;
  for (const stat of stats) {
    if (Number.isFinite(stat.code_lines)) lines += stat.code_lines;
  }
  const top = stats.reduce<(typeof stats)[number] | null>(
    (best, stat) => (best === null || stat.percentage > best.percentage ? stat : best),
    null,
  );
  return {
    ...emptyMetricsInput(),
    loc: lines,
    loc_language: top?.language ?? null,
    loc_truncated: report.truncated === true,
  };
}

/**
 * Disk totals, and how much of it is reclaimable.
 *
 * "Reclaimable" is build output plus caches — the two buckets the Storage view
 * itself offers to delete. Git internals are reported separately rather than
 * folded in: a large packfile is the repository, not junk.
 */
export function metricsFromStorage(report: StorageReport): FleetMetricsInput {
  const totals = report.totals;
  return {
    ...emptyMetricsInput(),
    storage_bytes: totals.grand_bytes,
    storage_git_bytes: totals.git_dir_bytes,
    storage_reclaimable_bytes: totals.build_artifacts_bytes + totals.cache_artifacts_bytes,
    storage_truncated: report.scan.truncated === true,
  };
}

/**
 * Vulnerability counts, and whether the audit actually finished.
 *
 * `audit_complete` is the load-bearing field. A repository where `npm` is
 * missing, or where one of several audit targets failed, reports zero
 * vulnerabilities — and zero from an audit that did not run must never render
 * the same as zero from one that did.
 */
export function metricsFromHealth(report: DepsHealthReport): FleetMetricsInput {
  const audit = report.audit;
  return {
    ...emptyMetricsInput(),
    vulns_critical: audit.critical,
    vulns_high: audit.high,
    vulns_moderate: audit.moderate,
    vulns_low: audit.low,
    vulns_unknown: audit.unknown ?? 0,
    vulns_total: audit.total,
    health_complete: report.audit_complete === true,
  };
}

export function metricsFromCoverage(report: CoverageReport): FleetMetricsInput {
  return {
    ...emptyMetricsInput(),
    coverage_pct: report.overall.percentage,
    coverage_truncated: report.truncated === true,
  };
}

/**
 * Runs one family's scan against one repository and returns what to record.
 *
 * Each branch names its command literally so the IPC contract checker can see
 * it; the family switch selects a branch, never a command string.
 */
export async function scanRepoFamily(
  family: ScanFamily,
  repoPath: string,
  invokeFn: InvokeFn = invoke,
): Promise<FleetMetricsInput> {
  switch (family) {
    case "loc":
      return metricsFromLanguages(
        await invokeFn<LanguageStatsReport>("cmd_get_language_stats", { repoPath }),
      );
    case "storage":
      return metricsFromStorage(await invokeFn<StorageReport>("cmd_storage_scan", { repoPath }));
    case "health":
      return metricsFromHealth(await invokeFn<DepsHealthReport>("cmd_scan_deps_health", { repoPath }));
    case "coverage":
      return metricsFromCoverage(await invokeFn<CoverageReport>("cmd_scan_coverage", { repoPath }));
  }
}
