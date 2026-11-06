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
  /**
   * This directory's own tree was enumerated to the end, so `bytes` is exact
   * and nothing inside it went unseen. Absent or false means a budget cut the
   * tree short: it is not a removal candidate, whatever the git probes said.
   * Distinct from `scan.truncated`, which says the repository's totals are
   * floors and may be true for reasons unrelated to this tree.
   */
  scan_complete?: boolean;
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
   * Locations a non-permission IO error (or a git probe with no answer) kept
   * the scan from reading. Their bytes are in no total, and `truncated` is set
   * whenever this is above zero. `permission_denied` locations are floors too.
   */
  unreadable: number;
  /** Entries with a non-UTF-8 name. Walked and counted; informational only. */
  non_utf8: number;
  /**
   * Items an audit cap kept from being examined (worktrees past the sizing
   * cap, orphan-audit entries past its cap). `truncated` is set when above zero.
   */
  unaudited: number;
  /**
   * True when any budget cut the scan short, or `unreadable` locations were
   * left out: totals are floors then, never render them as complete.
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

/** Mirrors the Rust `ConfigScope`: the config layer a value was read from. */
export type ConfigScope = "system" | "global" | "local" | "worktree" | "command" | "unknown";

/** Mirrors the Rust `TuningChange`: the three changes the apply may make. */
export type TuningChange = "many_files" | "fsmonitor" | "maintenance";

/** Mirrors the Rust `IndexReadState`; `unknown` is "could not read", never "missing". */
export type IndexReadState = "present" | "missing" | "unknown";

/** Global-config maintenance registration. `null` is unknown, never "not registered". */
export interface MaintenanceState {
  registered: boolean | null;
  strategy: string | null;
  auto: string | null;
}

/** Pack and loose-object pressure against `gc.auto`. */
export interface ObjectPressure {
  packs: number | null;
  loose_objects: number | null;
  /** `loose_objects` came from `git count-objects -v`; otherwise git's own estimate. */
  loose_exact: boolean;
  gc_auto: number | null;
  loose_over_gc_auto: boolean | null;
}

/** What the index header says (`DIRC`, version, entry count). */
export interface IndexInfo {
  state: IndexReadState;
  version: number | null;
  /** The tracked-file count from the header. */
  entries: number | null;
  /** False under split-index or a sparse index, where the header counts only part. */
  entries_exact: boolean;
  skip_hash: boolean | null;
  note: string | null;
}

/** One tuning key's value and the config layer that supplied it. */
export interface TuningKeyValue {
  key: string;
  /** `null`: unset, or unknown when `config_note` is set. */
  value: string | null;
  origin: ConfigScope | null;
}

/** Read-only repository tuning census (storage/repo_tuning.rs). */
export interface RepoTuningReport {
  maintenance: MaintenanceState;
  midx: boolean | null;
  objects: ObjectPressure;
  index: IndexInfo;
  partial_clone: boolean | null;
  sparse_checkout: boolean | null;
  keys: TuningKeyValue[];
  config_note: string | null;
  notes: string[];
}

/** One change worth offering, and why. */
export interface TuningOffer {
  change: TuningChange;
  title: string;
  reason: string;
}

/** The detector's report with the offers that clear their measured thresholds. */
export interface TuningAdvice {
  report: RepoTuningReport;
  offers: TuningOffer[];
  /** Said on their own, e.g. a `core.splitIndex` / `feature.manyFiles` conflict. */
  warnings: string[];
  /** An apply is running in this repository right now (set by the storage scan). */
  apply_in_flight?: boolean;
}

/** Where a change writes: the repository's config, the user's, or the OS scheduler. */
export type TuningScope = "repo_config" | "global_config" | "os_scheduler";

export interface TuningWrite {
  scope: TuningScope;
  what: string;
}

export interface TuningStep {
  change: TuningChange;
  /** The exact line, program first. Empty when `skipped`. */
  argv: string[];
  writes: TuningWrite[];
  /** Each command that reverses it, program first. */
  undo: string[][];
  undo_note: string | null;
  /** Facts the person must see before confirming. */
  notes: string[];
  /** Why nothing runs for this change (already in place). */
  skipped: string | null;
}

/** What a click on "Apply" would run (`cmd_tuning_plan`). */
export interface TuningPlan {
  common_dir: string;
  steps: TuningStep[];
  timeout_secs: number;
}

export type TuningStepStatus = "applied" | "skipped" | "failed" | "not_run";

export interface TuningStepResult {
  change: TuningChange;
  argv: string[];
  status: TuningStepStatus;
  detail: string | null;
}

/** How a confirmed apply ended (`cmd_tuning_apply`). */
export interface TuningOutcome {
  cancelled: boolean;
  joined: boolean;
  elapsed_ms: number;
  results: TuningStepResult[];
  /** The detector's answer after the run. */
  advice: TuningAdvice;
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
  /** Repository tuning census and offers; absent before the advisor existed or when the git dir could not be resolved. */
  tuning?: TuningAdvice | null;
  scan: ScanStats;
}
