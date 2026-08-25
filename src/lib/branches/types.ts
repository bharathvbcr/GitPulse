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
