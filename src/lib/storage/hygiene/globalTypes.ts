/** Opt-in reclaim scopes. Omitted from the wire while every field is at its default. */
export interface ReclaimScopes { worktree_caches: boolean; per_tree_vetoes: boolean; prune_worktrees: boolean; worktree_idle_days: number; }
/** The build-cache clone budget; 0 is no limit for both fields. Omitted from the wire at its default. */
export interface CloneBudget { max_target_clones: number; max_private_bytes: number; }
export interface CleanerConfig {
  version: number;
  revision: number;
  roots: string[];
  exclusions: string[];
  enabled: boolean;
  run_when_closed: boolean;
  interval_hours: number;
  next_run_at: number;
  retention_days: number;
  max_run_bytes: number;
  max_targets: number;
  scopes?: ReclaimScopes;
  clone_budget?: CloneBudget;
}
/** `scope` is "repository", "worktree-cache" or "prune-worktree"; for the last, `path` is the worktree's absolute path. */
export interface CleanerCandidate { repo_path: string; path: string; provider: string; bytes: number; scope: string; private_bytes?: number; }
export interface CleanerInventory { candidates: CleanerCandidate[]; repositories: number; visited_entries: number; partial: boolean; issues: string[]; }
export interface CleanerItem { repo_path: string; path: string; status: string; message: string; bytes_before: number; bytes_after: number | null; }
export interface CleanerRun { id: string; trigger: string; started_at: number; finished_at: number; status: string; repositories: number; partial: boolean; issues: string[]; items: CleanerItem[]; unlisted?: number; unexamined?: number; }
export interface CleanerState { config: CleanerConfig; history: CleanerRun[]; running: boolean; supported: boolean; background_supported: boolean; background_error: string | null; agent_rules: string; scopes_previewed: boolean; }
