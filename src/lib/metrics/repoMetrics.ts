/**
 * The repository metrics GitPulse keeps current by itself.
 *
 * Three of the four headline numbers had no revalidation at all. The fourth,
 * change count, already did: `cmd_branch_stats` is refetched by
 * `repoStore.handleRepoChanged`, so churn tracked the repository before this
 * module existed and is deliberately not duplicated here — it is refreshed by
 * the store that owns it, and re-measuring it separately would run the same
 * git subprocesses twice per change.
 *
 * What is defined here is the rest:
 *
 * | metric   | command                   | was                                    |
 * |----------|---------------------------|----------------------------------------|
 * | loc      | `cmd_get_language_stats`  | fetched once per repo-path change      |
 * | coverage | `cmd_scan_coverage`       | fetched twice, by two panels, once each|
 * | storage  | `cmd_storage_scan`        | once per path change, else manual only |
 *
 * ## Choosing the cost class
 *
 * The debounce and interval numbers below are not arbitrary; each is derived
 * from what the backend command actually costs:
 *
 * * **loc** reads up to ten thousand tracked files (≤1 MiB each) under a
 *   deadline. Expensive enough not to run per keystroke, cheap enough that a
 *   developer should see their line count move within a commit or two.
 * * **coverage** is content-fingerprinted in Rust — a rescan with unchanged
 *   artifacts re-walks the candidate directories but reuses every parse — so
 *   the repeat cost is dominated by discovery, not parsing. The interval is
 *   still generous because the *first* scan of a large tree is not cheap.
 * * **storage** walks the entire worktree and the git directory with a
 *   20-second deadline. This is by far the most expensive thing the app does
 *   on a timer, so it gets the longest floor. A build that writes continuously
 *   into `target/` must not be able to hold a scan permanently in flight.
 *
 * Pressing Rescan always bypasses the floor: an explicit user action that
 * visibly does nothing is worse than a duplicated scan.
 */

import { invoke } from "@tauri-apps/api/core";
import type { CoverageReport } from "../coverage/types";
import type { LanguageStatsReport } from "../language/barStats";
import type { StorageReport } from "../storage/types";
import { formatDiagnosticFailure, reportPanelError } from "../diagnostics/report";
import { createMetric, createMetricRegistry, type Metric } from "./freshness";

/**
 * Shared failure wiring for every metric.
 *
 * `formatDiagnosticFailure` is the app's one safe formatter: it handles hostile
 * thrown values and redacts credentials that a git or network error can carry.
 * Routing every metric through it keeps the banner text byte-identical to what
 * the panels showed when each owned its own catch block — and keeps the
 * failures in the diagnostics ring, which moving the fetch out of the panels
 * would otherwise have quietly stopped.
 */
const REPORTERS = {
  // Written out one literal per panel rather than `reportPanelError(source, …)`
  // so the diagnostics contract can still see them. That check scans for
  // `reportPanelError("<source>"` and fails a declared panel that records
  // nothing — routing the source through a variable made `storage` look
  // silent, which is exactly the drift the contract exists to catch.
  pulse: (message: string) => reportPanelError("pulse", message),
  coverage: (message: string) => reportPanelError("coverage", message),
  storage: (message: string) => reportPanelError("storage", message),
} as const;

function failureWiring(source: keyof typeof REPORTERS) {
  return {
    formatError: (err: unknown) => formatDiagnosticFailure(err),
    onFailure: (_repoPath: string, message: string) => {
      REPORTERS[source](message);
    },
  };
}

/** Matches `repoPanelCache`'s bound, so the two age out together. */
const MAX_TRACKED_REPOS = 8;

/**
 * Language and line-count statistics.
 *
 * The whole report is the measurement, not just the summed line count: the
 * headline LOC number and the language bar are two readings of one scan, and
 * deriving them separately is how they came to disagree.
 */
export const locMetric: Metric<LanguageStatsReport> = createMetric<LanguageStatsReport>({
  name: "loc",
  measure: (repoPath) => invoke<LanguageStatsReport>("cmd_get_language_stats", { repoPath }),
  debounceMs: 1_500,
  minIntervalMs: 20_000,
  // The backend says so itself when its deadline or its 10k-file cap fired.
  // A truncated report is a floor, and must never render as a total.
  isPartial: (report) => report?.truncated === true,
  maxRepos: MAX_TRACKED_REPOS,
  ...failureWiring("pulse"),
});

/**
 * The coverage report, measured once for every panel that wants it.
 *
 * `PulseView` and `CoverageViewer` each used to invoke `cmd_scan_coverage`
 * independently, keep their own cached report, and reach their own conclusion
 * about whether coverage had loaded. Two panels open on the same repository
 * could show different numbers.
 */
export const coverageMetric: Metric<CoverageReport> = createMetric<CoverageReport>({
  name: "coverage",
  measure: (repoPath) => invoke<CoverageReport>("cmd_scan_coverage", { repoPath }),
  debounceMs: 2_000,
  minIntervalMs: 30_000,
  isPartial: (report) => report?.truncated === true,
  maxRepos: MAX_TRACKED_REPOS,
  ...failureWiring("coverage"),
});

/** Disk usage, the most expensive scan in the app. */
export const storageMetric: Metric<StorageReport> = createMetric<StorageReport>({
  name: "storage",
  measure: (repoPath) => invoke<StorageReport>("cmd_storage_scan", { repoPath }),
  debounceMs: 5_000,
  minIntervalMs: 120_000,
  isPartial: (report) => report?.scan?.truncated === true,
  maxRepos: MAX_TRACKED_REPOS,
  ...failureWiring("storage"),
});

/**
 * The registry the `repo-changed` listener talks to.
 *
 * One call per watcher event; each metric then applies its own debounce and
 * cost floor. Adding a metric here is all it takes for it to start tracking
 * the repository.
 */
export const repoMetrics = createMetricRegistry();
repoMetrics.register(locMetric as never);
repoMetrics.register(coverageMetric as never);
repoMetrics.register(storageMetric as never);

/** Total code lines in a report, or null when there is nothing measured. */
export function totalCodeLines(report: LanguageStatsReport | null): number | null {
  if (!report || !Array.isArray(report.stats)) return null;
  return report.stats.reduce((sum, stat) => sum + (stat.code_lines ?? 0), 0);
}
