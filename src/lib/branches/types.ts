export interface BranchInfo {
  name: string;
  is_current: boolean;
  is_remote: boolean;
  remote_name?: string | null;
  tip_commit_id: string;
  ahead_count: number;
  behind_count: number;
  upstream?: string | null;
  is_default: boolean;
  is_gone: boolean;
  last_commit_timestamp: number;
  last_author: string;
  last_summary: string;
  commits_ahead_of_base: number;
  commits_behind_base: number;
  additions: number;
  deletions: number;
  files_changed: number;
  compared_to?: string | null;
}

export interface DeadbranchConfig {
  days_threshold?: number;
  protected_branches?: string[];
  exclude_patterns?: string[];
  include_remote?: boolean;
  merged_only?: boolean;
  check_squash?: boolean;
}

export type DeadbranchSeverity = "fresh" | "moderate" | "stale";

export interface StaleBranchInfo {
  name: string;
  short_name: string;
  age_days: number;
  severity: DeadbranchSeverity;
  is_merged: boolean;
  merged_by_tree: boolean;
  is_remote: boolean;
  is_protected: boolean;
  is_wip: boolean;
  is_current_or_worktree: boolean;
  last_commit_sha: string;
  last_commit_timestamp: number;
  last_author: string;
  last_summary: string;
}

export interface DeadbranchScanResult {
  default_branch: string;
  branches: StaleBranchInfo[];
  total_scanned: number;
  stale_count: number;
  merged_count: number;
  squash_merged_count: number;
  warnings: string[];
}

export interface DeadbranchCleanResult {
  deleted: string[];
  failed: [string, string][];
  backup_path: string | null;
  message: string;
}

export interface DeadbranchBackupInfo {
  filename: string;
  path: string;
  timestamp: number;
  branch_count: number;
  repo_name: string;
}

export interface DeadbranchRestoreResult {
  restored: string[];
  failed: [string, string][];
  message: string;
}

export interface TagInfo {
  name: string;
  commit_id: string;
  message?: string | null;
  /** Commits on the tag but not on `compared_to`. Meaningless without it. */
  commits_ahead_of_base: number;
  commits_behind_base: number;
  /**
   * Base the counts are measured against, or null when the comparison could
   * not be made. Null is "not asked", zero-ahead is "already merged" — never
   * render the first as the second.
   */
  compared_to?: string | null;
}

/** Wire shape of `cmd_list_tags`. A bare tag array could not say when the cap cut older tags. */
export interface TagList {
  tags: TagInfo[];
  truncated: boolean;
}

/**
 * Unwraps a `cmd_list_tags` payload.
 *
 * A bare array, a missing `truncated` flag, or a malformed tag is a failed
 * read — not an empty tag list. Folding those into `tags: []` is how a
 * 10k-tag repo, or a probe that threw, comes to look like "no tags".
 */
export function parseTagList(value: unknown): {
  tags: TagInfo[];
  truncated: boolean;
  failed: boolean;
} {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return { tags: [], truncated: false, failed: true };
  }
  const rec = value as { tags?: unknown; truncated?: unknown };
  if (!Array.isArray(rec.tags) || typeof rec.truncated !== "boolean") {
    return { tags: [], truncated: false, failed: true };
  }
  const tags: TagInfo[] = [];
  for (const item of rec.tags) {
    if (!item || typeof item !== "object" || Array.isArray(item)) {
      return { tags: [], truncated: false, failed: true };
    }
    const t = item as {
      name?: unknown;
      commit_id?: unknown;
      message?: unknown;
      commits_ahead_of_base?: unknown;
      commits_behind_base?: unknown;
      compared_to?: unknown;
    };
    if (typeof t.name !== "string" || typeof t.commit_id !== "string") {
      return { tags: [], truncated: false, failed: true };
    }
    // Present-but-wrong is a corrupt payload, same as a missing name. Absent
    // is a payload from a build that did not measure tags: that must land as
    // "not compared" (compared_to null), never as "zero commits ahead".
    const ahead = t.commits_ahead_of_base;
    const behind = t.commits_behind_base;
    if (
      (ahead !== undefined && typeof ahead !== "number") ||
      (behind !== undefined && typeof behind !== "number") ||
      (t.compared_to !== undefined && t.compared_to !== null && typeof t.compared_to !== "string")
    ) {
      return { tags: [], truncated: false, failed: true };
    }
    tags.push({
      name: t.name,
      commit_id: t.commit_id,
      message: typeof t.message === "string" ? t.message : null,
      commits_ahead_of_base: typeof ahead === "number" ? ahead : 0,
      commits_behind_base: typeof behind === "number" ? behind : 0,
      compared_to: typeof t.compared_to === "string" ? t.compared_to : null,
    });
  }
  return { tags, truncated: rec.truncated, failed: false };
}

export interface BranchFolder {
  id: string;
  label: string;
  folders: BranchFolder[];
  branches: BranchInfo[];
}

export interface BranchSection {
  id: string;
  label: string;
  kind: "pinned" | "recent" | "local" | "remote" | "tags";
  remoteName?: string;
  folders: BranchFolder[];
  branches: BranchInfo[];
  tags: TagInfo[];
  branchCount: number;
}

export type BranchFilterTab = "all" | "local" | "remote" | "active" | "stale" | "tags" | "pinned";

/**
 * A single `git reflog` record. Lived inside ReflogViewer until `check:types`
 * grew wide enough to check it, which needs a module rather than a component.
 */
export interface ReflogEntry {
  index: number;
  commit_id: string;
  selector: string;
  action: string;
  message: string;
  timestamp: number;
}

/** One commit named in a `ResetPreview` (Rust `git_reader::PreviewCommit`). */
export interface PreviewCommit {
  commit_id: string;
  summary: string;
  author_name: string;
  timestamp: number;
}

/**
 * What resetting HEAD's branch to `target` would take off it (Rust
 * `git_reader::ResetPreview`). `leaving` is capped; `leaving_total` is not.
 */
export interface ResetPreview {
  /** Null when HEAD is detached: only HEAD moves. */
  branch: string | null;
  head: string;
  /** The target peeled to a commit oid — reset to this, not the name. */
  target: string;
  leaving: PreviewCommit[];
  leaving_total: number;
  /** Leaving commits no other branch, tag or remote ref reaches. */
  unreachable_total: number;
  gaining_total: number;
}

export interface WorktreeDiffStat {
  files_changed: number;
  insertions: number;
  deletions: number;
}

export interface WorktreeDivergence {
  ahead: number;
  behind: number;
}

export interface WorktreeRouteInfo {
  hostname: string;
  port: number;
  url: string;
  is_portless: boolean;
  is_listening: boolean;
  pid: number | null;
}

/** One linked worktree, as reported by `git worktree list --porcelain`. */
export interface WorktreeInfo {
  path: string;
  name: string;
  head: string;
  branch: string | null;
  is_bare: boolean;
  is_detached: boolean;
  is_main: boolean;
  is_locked: boolean;
  is_prunable: boolean;
  dirty_files: number | null;
  diff_stat: WorktreeDiffStat | null;
  main_divergence: WorktreeDivergence | null;
  active_routes: WorktreeRouteInfo[];
  /**
   * Why measurements are absent or partial: a bare entry, one past the scan
   * limit, a missing directory, or a read that failed or was deferred under
   * load. Absent when everything was measured.
   */
  scan_note?: string;
}

/**
 * `engine::worktree::MergeTeardownResult`. A result is only ever one where the
 * merge landed; cleanup that did not finish after it is `worktree_removed` /
 * `branch_deleted` false with the reason beside it, not a failure.
 */
export interface MergeTeardownResult {
  merged: boolean;
  merged_branch: string;
  target_branch: string;
  commits_merged: number;
  /** The target's tip before the merge, and after it (null when git could not say). */
  before_oid: string;
  after_oid: string | null;
  /** The commit the lane was cut from, when the lane recorded one. */
  base_oid: string | null;
  worktree_removed: boolean;
  removal_error: string | null;
  branch_deleted: boolean;
  branch_error: string | null;
  /** A `post_merge` hook failed after the merge and teardown had happened. */
  hook_error: string | null;
  /** Hooks the repository lists were not approved, so did not run. */
  hook_notice?: string | null;
  /**
   * The worktree is removed (`worktree_removed` true) but the files moved
   * aside for deletion could not all be deleted, or a leftover from an earlier
   * teardown could not be cleared. Names the path; the next teardown retries.
   */
  cleanup_error?: string | null;
  /**
   * Leftovers of earlier teardowns that the sweep before this one could not
   * clear (and a cap that was hit): about the repository, not this removal.
   * At most 17 short notes.
   */
  sweep_problems?: string[];
}

/**
 * `engine::worktree::WorktreeRemoval`: what the Worktrees panel's delete
 * (`cmd_remove_worktree`) returns as its `output`. The worktree is removed
 * whenever the call resolved; this is only about the files after it.
 */
export interface WorktreeRemoval {
  cleanup_error?: string | null;
  /** As on {@link MergeTeardownResult.sweep_problems}. */
  sweep_problems?: string[];
}

/**
 * `engine::worktree::MergeTeardownError`: why a merge-and-teardown did not
 * merge, as `{code, message, ...}`. Every variant carries a `message` to show.
 */
export type MergeTeardownError =
  /** The merge conflicted. `rolled_back` false means the main checkout is still mid-merge (`rollback_error` says what is left). */
  | { code: "conflict"; message: string; paths: string[]; total: number; rolled_back: boolean; rollback_error: string | null }
  /** The branch is not at the commit that was checked; nothing was merged. */
  | { code: "moved"; message: string; branch: string; expected: string; actual: string }
  /** The worktree was cut from a branch or base the target no longer is. */
  | { code: "base_mismatch"; message: string; base_branch: string; target_branch: string; base_oid: string }
  /** A squash is staged in the main checkout and not committed. */
  | { code: "squash_parked"; message: string }
  /** Refused or failed before anything was merged. */
  | { code: "refused"; message: string };

/** `engine::worktree_hooks::WorktreeHooksConfig`. */
export interface WorktreeHooksConfig {
  post_create: string[];
  pre_merge: string[];
  post_merge: string[];
}

/**
 * The repository's `.gitpulse/hooks` file as the approval dialog shows it.
 * Mirrors `engine::worktree_hooks::WorktreeHooksPreview`. `approved` is true
 * only for this exact text; any edit asks again.
 */
export interface WorktreeHooksPreview {
  file: string;
  content: string;
  config: WorktreeHooksConfig;
  approved: boolean;
}

/**
 * A worktree that exists, and what of its optional setup did not happen.
 * Mirrors `engine::worktree::WorktreeCreated`.
 */
export interface WorktreeCreated {
  path: string;
  /** Some build cache could not be copied (or checked); each is named, and none is left half-copied. */
  cache_error: string | null;
  /** A `post_create` hook failed, timed out, or was refused. */
  hook_error: string | null;
  /** Hooks were asked for but the repository's file was not approved, so none ran. */
  hook_notice?: string | null;
}

/**
 * What to tell the reader after a worktree operation succeeded with a step
 * left undone: one sentence per step, empty when there is nothing to say.
 */
export function worktreeSetupNotices(
  done: {
    cache_error?: string | null;
    hook_error?: string | null;
    hook_notice?: string | null;
    worktree_removed?: boolean;
    removal_error?: string | null;
    branch_deleted?: boolean;
    branch_error?: string | null;
    cleanup_error?: string | null;
    sweep_problems?: string[];
  },
  when: "create" | "merge" | "remove",
): string[] {
  const notices: string[] = [];
  if (when !== "create" && done.cleanup_error) notices.push(done.cleanup_error);
  if (when !== "create" && done.sweep_problems?.length) {
    notices.push(`Leftover files from an earlier worktree removal could not be cleared: ${done.sweep_problems.join("; ")}`);
  }
  if (when === "merge" && done.worktree_removed === false) {
    notices.push(`Merged, but the worktree was not removed: ${done.removal_error ?? "no reason was given"}. The merge stands; remove the worktree when it is free.`);
  } else if (when === "merge" && done.branch_deleted === false) {
    notices.push(`Merged and the worktree was removed, but the branch was kept: ${done.branch_error ?? "no reason was given"}`);
  }
  if (done.cache_error) notices.push(`The worktree was created, but its build caches were not copied: ${done.cache_error}`);
  if (done.hook_error) {
    notices.push(when === "create"
      ? `The worktree was created, but its post_create hook did not finish, so it may not be set up: ${done.hook_error}`
      : `The merge and teardown finished, but the post_merge hook did not: ${done.hook_error}`);
  }
  if (done.hook_notice) notices.push(done.hook_notice);
  return notices;
}

export interface ReflinkResult {
  source: string;
  destination: string;
  files_cloned: number;
  bytes_cloned: number;
  is_cow: boolean;
  duration_ms: number;
}

