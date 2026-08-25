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
  scan: ScanStats;
}
