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

export interface TagInfo {
  name: string;
  commit_id: string;
  message?: string | null;
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
}
