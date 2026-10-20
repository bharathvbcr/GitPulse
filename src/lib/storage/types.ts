/**
 * TypeScript mirror of `crate::storage` (`src-tauri/src/storage/mod.rs`).
 * Field names stay snake_case across the IPC seam, matching every other
 * command payload in this app.
 */

export interface StorageTotals {
  worktree_bytes: number;
  git_dir_bytes: number;
  grand_bytes: number;
  build_artifacts_bytes: number;
  cache_artifacts_bytes: number;
}

export type ArtifactKind = "build" | "cache";

/** One build-output or cache directory found in the working tree. */
export interface ArtifactDir {
  path: string;
  bytes: number;
  kind: ArtifactKind;
  /** True when NO ignore rule covers this directory. */
  unignored: boolean;
  /** Index-tracked files inside an artifact dir: committed-cache bloat. */
  tracked_files: number;
  /**
   * The ignore or tracked-file probe did not run. Zero `tracked_files` and
   * `unignored: false` are then unknown, not a finding that the directory
   * is disposable.
   */
  checks_unexamined?: boolean;
}

export interface LargeFile {
  path: string;
  bytes: number;
}

export interface WorktreeUsage {
  path: string;
  name: string;
  branch: string | null;
  bytes: number;
  truncated: boolean;
}

export interface GitStorage {
  pack_bytes: number;
  pack_file_count: number;
  loose_bytes: number;
  loose_object_count: number;
  refs_bytes: number;
  reflog_bytes: number;
  lfs_bytes: number;
  modules_bytes: number;
  worktrees_admin_bytes: number;
  index_bytes: number;
  other_bytes: number;
  total_bytes: number;
  gc_recommended: boolean;
}

export interface BranchStorageSummary {
  local_count: number;
  remote_tracking_count: number;
  merged_stale_count: number;
  gone_upstream_count: number;
  sample_merged_stale: string[];
  sample_gone_upstream: string[];
  error: string | null;
}

export type ReclaimConfidence = "measured" | "estimated";

export type ReclaimSafety = "safe" | "needs_review";

export type ReclaimCategory =
  | "build_output"
  | "cache"
  | "git_objects"
  | "reflog"
  | "orphaned_worktree_admin"
  | "merged_branches"
  | "large_file";

export interface ReclaimItem {
  category: ReclaimCategory;
  label: string;
  bytes: number;
  confidence: ReclaimConfidence;
  safety: ReclaimSafety;
  action: string;
  detail: string;
  blocked_reason: string | null;
}

export interface ReclaimSummary {
  reclaimable_bytes: number;
  estimated_bytes: number;
  needs_review_bytes: number;
  item_count: number;
  partial: boolean;
}

export interface ScanStats {
  elapsed_ms: number;
  files_visited: number;
  permission_denied: number;
  /**
   * True when any budget cut the scan short: totals are floors then,
   * never render them as complete.
   */
  truncated: boolean;
}

/** Mirrors the Rust `CommitGraphState`; `unknown` is "could not check", never "missing". */
export type CommitGraphState =
  | "present"
  | "no_bloom"
  | "stale"
  | "missing"
  | "no_history"
  | "unknown";

/** Mirrors the Rust `CommitGraphLayout`. */
export type CommitGraphLayout = "none" | "single" | "split";

/** Read-only census of the repository's commit-graph (storage/commit_graph.rs). */
export interface CommitGraphReport {
  state: CommitGraphState;
  layout: CommitGraphLayout;
  layers: number;
  commits: number | null;
  bloom: boolean | null;
  head_covered: boolean | null;
  note: string | null;
  /** A commit-graph write is running in this repository right now (set by the storage scan). */
  write_in_flight?: boolean;
}

/** What a click on "Write commit-graph" would run (`cmd_commit_graph_write_plan`). */
export interface CommitGraphWritePlan {
  /** The exact line the gate judges and git is started with, program first. */
  argv: string[];
  /** The directory whose `objects/info` is written, shared by every worktree. */
  common_dir: string;
  state: CommitGraphState;
  /** The run is killed after this many seconds. */
  timeout_secs: number;
}

/** How a confirmed write ended (`cmd_commit_graph_write`). */
export interface CommitGraphWriteOutcome {
  cancelled: boolean;
  /** This click joined a write another click had already started. */
  joined: boolean;
  elapsed_ms: number;
  /** The detector's answer after the run. */
  report: CommitGraphReport;
  /** Set when a lock file remains after a stopped write: its path, age and remedy. */
  note: string | null;
}

export interface StorageReport {
  repo_path: string;
  generated_at_epoch_secs: number;
  is_bare: boolean;
  totals: StorageTotals;
  git: GitStorage;
  artifacts: ArtifactDir[];
  largest_files: LargeFile[];
  worktrees: WorktreeUsage[];
  branches: BranchStorageSummary;
  reclaim: ReclaimItem[];
  reclaim_summary: ReclaimSummary;
  /** Absent in payloads from before the detector existed. */
  commit_graph?: CommitGraphReport;
  scan: ScanStats;
}
