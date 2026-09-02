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
    const t = item as { name?: unknown; commit_id?: unknown; message?: unknown };
    if (typeof t.name !== "string" || typeof t.commit_id !== "string") {
      return { tags: [], truncated: false, failed: true };
    }
    tags.push({
      name: t.name,
      commit_id: t.commit_id,
      message: typeof t.message === "string" ? t.message : null,
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
