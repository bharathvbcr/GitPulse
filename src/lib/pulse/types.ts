/**
 * Type definitions for repository Pulse metrics, IPC reports, and derived calculations.
 */

export interface PulseCommitSummary {
  sha: string;
  parents: string[];
  timestamp: number;
  summary: string;
  author_name: string;
  author_email: string;
  gpg_status: string;
  additions: number;
  deletions: number;
  files_changed: number;
  is_merge: boolean;
  is_revert: boolean;
  co_authors: string[];
  binary_files: number;
}

export interface PulseFileChurn {
  path: string;
  additions: number;
  deletions: number;
  commits_count: number;
}

export interface PulseExtensionChurn {
  extension: string;
  additions: number;
  deletions: number;
  files_count: number;
}

/** Wire report received from cmd_get_pulse_report */
export interface PulseReport {
  commits: PulseCommitSummary[];
  top_files_by_churn: PulseFileChurn[];
  extensions: PulseExtensionChurn[];
  has_mailmap: boolean;
  total_commits_scanned: number;
  truncated: boolean;
  payload_truncated: boolean;
  duration_ms: number;
}

/** Heatmap Day bucket for 53x7 calendar */
export interface HeatmapDay {
  readonly date: string; // YYYY-MM-DD
  readonly dayOfWeek: number; // 0 = Sun .. 6 = Sat
  readonly timestamp: number; // epoch ms
  readonly count: number;
  readonly additions: number;
  readonly deletions: number;
  readonly churn: number;
  /** Activity tier: 0 (none) .. 4 (peak) */
  readonly level: number;
}

/** Heatmap Week column (Sunday to Saturday) */
export interface HeatmapWeek {
  readonly weekIndex: number;
  readonly days: readonly (HeatmapDay | null)[];
}

/** Streak and rhythm metrics */
export interface RhythmStats {
  readonly currentStreak: number;
  readonly longestStreak: number;
  readonly activeDaysInWindow: number;
  readonly totalDaysInWindow: number;
  readonly longestInactiveGap: number;
}

/** Punch card cell (hour of day x day of week) */
export interface PunchCardCell {
  readonly dayOfWeek: number; // 0 = Sun .. 6 = Sat
  readonly hour: number; // 0 .. 23
  readonly count: number;
  readonly churn: number;
}

export interface PunchCardStats {
  readonly cells: readonly PunchCardCell[];
  readonly maxCount: number;
  readonly maxChurn: number;
  readonly totalCommits: number;
  readonly afterHoursCommits: number;
  readonly afterHoursPercentage: number;
}

/** Weekly Line change bucket */
export interface WeeklyLineBucket {
  readonly weekStart: string; // YYYY-MM-DD
  readonly timestamp: number;
  readonly additions: number;
  readonly deletions: number;
  readonly net: number;
}

/** Historical LOC Trend point */
export interface LocTrendPoint {
  readonly date: string; // YYYY-MM-DD
  readonly timestamp: number;
  readonly totalLoc: number;
}

/** Commit hygiene and engineering quality metrics */
export interface HygieneStats {
  readonly totalCommits: number;
  readonly conventionalCount: number;
  readonly conventionalPercentage: number;
  readonly signedCount: number;
  readonly signedPercentage: number;
  readonly mergeCount: number;
  readonly mergePercentage: number;
  readonly revertCount: number;
  readonly medianChurn: number;
  readonly coAuthorCount: number;
  readonly coAuthorPercentage: number;
}

/** Wire author ownership record */
export interface AuthorOwnership {
  author_name: string;
  author_email: string;
  lines_owned: number;
  percentage: number;
}

/** Wire orphaned file record */
export interface OrphanedFile {
  path: string;
  primary_author: string;
  author_email: string;
  lines_count: number;
  last_commit_timestamp: number;
}

/** Wire code age line distribution */
export interface CodeAgeDistribution {
  fresh_lines: number;
  recent_lines: number;
  maturing_lines: number;
  legacy_lines: number;
  ancient_lines: number;
  total_lines: number;
}

/** Wire report received from cmd_get_knowledge_report */
export interface KnowledgeReport {
  scanned_files: number;
  candidate_files: number;
  scanned_lines: number;
  bus_factor: number;
  primary_authors: AuthorOwnership[];
  orphaned_files: OrphanedFile[];
  age_distribution: CodeAgeDistribution;
  half_life_days: number;
  truncated: boolean;
  duration_ms: number;
}

/** Wire report received from cmd_get_dora_report */
export interface DoraReport {
  deploy_frequency_per_week: number;
  deploy_rating: string;
  total_releases: number;
  median_lead_time_hours: number;
  lead_time_rating: string;
  change_failure_rate_pct: number;
  is_cfr_approximation: boolean;
  mttr_hours: number;
  is_mttr_approximation: boolean;
  window_days: number;
}

/** Wire input for saving a pulse snapshot */
export interface PulseSnapshotInput {
  day: string;
  total_commits: number;
  total_loc: number;
  bus_factor: number;
  coverage_pct: number | null;
  snapshot_json: string;
}

/** Wire entry for historical pulse snapshot from ledger */
export interface PulseSnapshotEntry {
  id: number;
  day: string;
  repo_path: string;
  total_commits: number;
  total_loc: number;
  bus_factor: number;
  coverage_pct: number | null;
  snapshot_json: string;
}

/** Derived hotspot item joining churn and coverage */
export interface HotspotRiskItem {
  readonly path: string;
  readonly churn: number;
  readonly additions: number;
  readonly deletions: number;
  readonly commitsCount: number;
  readonly coveragePercentage: number | null;
  readonly linesFound: number | null;
  readonly uncoveredLines: number | null;
  readonly riskScore: number;
  readonly riskLevel: "critical" | "high" | "medium" | "low";
  /** How coverage was joined. Unscanned must never render as untested. */
  readonly coverageStatus: "hit" | "missing-file" | "unscanned";
}

/** 30d vs prior 30d comparison deltas */
export interface PeriodCompareDeltas {
  readonly currentCommits: number;
  readonly priorCommits: number;
  readonly commitsDeltaPct: number;
  readonly currentAdds: number;
  readonly priorAdds: number;
  readonly addsDeltaPct: number;
  readonly currentDels: number;
  readonly priorDels: number;
  readonly delsDeltaPct: number;
  readonly currentActiveDays: number;
  readonly priorActiveDays: number;
  readonly activeDaysDelta: number;
}

