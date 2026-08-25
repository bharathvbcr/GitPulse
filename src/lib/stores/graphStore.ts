import { writable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import type { VisualCommitRow } from "../canvas/GraphRenderer";
import {
  DEFAULT_MAX_COMMITS,
  nextLoadLimit,
} from "./graphLimits";

export type InvokeFn = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

export interface CommitFileChange {
  path: string;
  status_code: string;
  additions: number;
  deletions: number;
}

export interface CommitDetails {
  id: string;
  parent_ids: string[];
  author_name: string;
  author_email: string;
  author_date: string;
  committer_name: string;
  committer_email: string;
  committer_date: string;
  summary: string;
  body: string;
  gpg_status: string;
  co_authors: string[];
  changed_files: CommitFileChange[];
  total_additions: number;
  total_deletions: number;
  /**
   * Present only when the backend capped the changed-file list for a massive
   * commit; absent otherwise, so every consumer must default gracefully.
   */
  files_total_count?: number;
  files_list_truncated?: boolean;
}

/** A file left out of a truncated commit diff, stats only. */
export interface SkippedFileStat {
  path: string;
  additions: number;
  deletions: number;
}

/** Wire shape of `cmd_get_commit_diff` (snake_case over IPC). */
export interface DiffPayload {
  content: string;
  truncated: boolean;
  included_files: number;
  skipped_files: SkippedFileStat[];
  total_files: number;
  total_additions: number;
  total_deletions: number;
}

function coerceCount(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0
    ? value
    : 0;
}

function coerceSkippedFiles(value: unknown): SkippedFileStat[] {
  if (!Array.isArray(value)) return [];
  const files: SkippedFileStat[] = [];
  for (const entry of value) {
    if (entry === null || typeof entry !== "object" || Array.isArray(entry)) continue;
    const record = entry as Record<string, unknown>;
    if (typeof record.path !== "string") continue;
    files.push({
      path: record.path,
      additions: coerceCount(record.additions),
      deletions: coerceCount(record.deletions),
    });
  }
  return files;
}

function emptyDiffPayload(): DiffPayload {
  return {
    content: "",
    truncated: false,
    included_files: 0,
    skipped_files: [],
    total_files: 0,
    total_additions: 0,
    total_deletions: 0,
  };
}

/**
 * Defensively turns whatever crossed `invoke()` into a `DiffPayload`. The
 * backend is transitioning from a bare diff string to this object, and other
 * stores still hold `string | null`-typed diff state, so every wrong shape
 * degrades to a safe default instead of throwing downstream: a legacy string
 * becomes an untruncated payload; null, arrays and other junk become empty.
 */
export function normalizeDiffPayload(raw: unknown): DiffPayload {
  if (typeof raw === "string") {
    return { ...emptyDiffPayload(), content: raw };
  }
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    return emptyDiffPayload();
  }
  const record = raw as Record<string, unknown>;
  return {
    content: typeof record.content === "string" ? record.content : "",
    truncated: record.truncated === true,
    included_files: coerceCount(record.included_files),
    skipped_files: coerceSkippedFiles(record.skipped_files),
    total_files: coerceCount(record.total_files),
    total_additions: coerceCount(record.total_additions),
    total_deletions: coerceCount(record.total_deletions),
  };
}

export interface FoldedBranchRun {
  merge_commit_id: string;
  branch_root_id: string;
  folded_commit_ids: string[];
  commit_count: number;
  is_collapsed: boolean;
}

/** A branch, remote branch, tag or detached HEAD pointing at a row. */
export interface RefDecoration {
  name: string;
  kind: "local" | "remote" | "tag" | "head";
  commit_id: string;
  is_head: boolean;
}

export interface CommitGraphPayload {
  rows: VisualCommitRow[];
  folds: FoldedBranchRun[];
  head_id: string | null;
  /**
   * Refs resolved by the backend in one pass over `for-each-ref`, rather than
   * derived in the UI from the branch list: an annotated tag has to be peeled
   * to the commit it points at before it can decorate a row, and a detached
   * HEAD belongs to no branch at all, so neither survives a derivation from
   * branch tips.
   */
  refs?: RefDecoration[];
  /** True when older commits exist beyond this page. */
  has_more?: boolean;
}

export interface GraphState {
  rows: VisualCommitRow[];
  commits: VisualCommitRow[];
  folds: FoldedBranchRun[];
  refs: RefDecoration[];
  headId: string | null;
  selectedCommit: VisualCommitRow | null;
  selectedCommitDetails: CommitDetails | null;
  isLoading: boolean;
  maxCommits: number;
  visiblePath: string | null;
  /** Why the last load failed, when it did. Never silently emptied. */
  error: string | null;
  /** True when older history exists past the current page. */
  hasMore: boolean;
}

interface CachedGraph {
  rows: VisualCommitRow[];
  commits: VisualCommitRow[];
  folds: FoldedBranchRun[];
  refs: RefDecoration[];
  headId: string | null;
  selectedCommit: VisualCommitRow | null;
  selectedCommitDetails: CommitDetails | null;
  query: string;
  revision: string | null;
  maxCommits: number;
  hasMore: boolean;
}

function emptyVisible(
  path: string | null,
  maxCommits: number,
  loading: boolean
): GraphState {
  return {
    rows: [],
    commits: [],
    folds: [],
    refs: [],
    headId: null,
    selectedCommit: null,
    selectedCommitDetails: null,
    isLoading: loading,
    maxCommits,
    visiblePath: path,
    error: null,
    hasMore: false,
  };
}

export function createGraphStore(deps: { invoke?: InvokeFn } = {}) {
  const invokeFn = deps.invoke ?? (invoke as InvokeFn);
  const cache = new Map<string, CachedGraph>();
  const generations = new Map<string, number>();
  /** Monotonic per-repo counter so a slow details fetch from an older
   * selection can never overwrite a newer one's pane. */
  const selections = new Map<string, number>();
  let visiblePath: string | null = null;

  const { subscribe, update, set } = writable<GraphState>(
    emptyVisible(null, DEFAULT_MAX_COMMITS, false)
  );

  function bump(path: string): number {
    const next = (generations.get(path) ?? 0) + 1;
    generations.set(path, next);
    return next;
  }

  function isCurrent(path: string, token: number): boolean {
    return generations.get(path) === token;
  }

  function selectionSeq(path: string): number {
    const next = (selections.get(path) ?? 0) + 1;
    selections.set(path, next);
    return next;
  }

  function selectionCurrent(path: string, seq: number): boolean {
    return selections.get(path) === seq;
  }

  function limitFor(repoPath: string): number {
    return cache.get(repoPath)?.maxCommits ?? DEFAULT_MAX_COMMITS;
  }

  async function fetchDetails(
    repoPath: string,
    token: number,
    commitId: string,
    seq?: number
  ) {
    try {
      const details = await invokeFn<CommitDetails>("cmd_get_commit_details", {
        repoPath,
        commitId,
      });
      if (!isCurrent(repoPath, token)) return;
      if (seq !== undefined && !selectionCurrent(repoPath, seq)) return;
      const cachedNow = cache.get(repoPath);
      if (cachedNow && cachedNow.selectedCommit?.id === commitId) {
        cachedNow.selectedCommitDetails = details;
      }
      if (visiblePath === repoPath) {
        update((s) =>
          s.selectedCommit?.id === commitId ? { ...s, selectedCommitDetails: details } : s
        );
      }
    } catch {
      /* details are best-effort; the row itself already renders */
    }
  }

  const api = {
    subscribe,
    showRepo: (path: string | null) => {
      visiblePath = path;
      if (!path) {
        set(emptyVisible(null, DEFAULT_MAX_COMMITS, false));
        return;
      }
      const cached = cache.get(path);
      if (cached) {
        update((s) => ({
          ...s,
          rows: cached.rows,
          commits: cached.commits,
          folds: cached.folds,
          refs: cached.refs,
          headId: cached.headId,
          selectedCommit: cached.selectedCommit,
          selectedCommitDetails: cached.selectedCommitDetails,
          maxCommits: cached.maxCommits,
          hasMore: cached.hasMore,
          error: null,
          isLoading: false,
          visiblePath: path,
        }));
        return;
      }
      update((s) => emptyVisible(path, s.maxCommits, true));
    },
    evict: (path: string) => {
      cache.delete(path);
      generations.set(path, (generations.get(path) ?? 0) + 1);
      if (visiblePath === path) {
        visiblePath = null;
        set(emptyVisible(null, DEFAULT_MAX_COMMITS, false));
      }
    },
    /**
     * Requests one page of history. The per-repository page size persists in
     * the cache, so "load more" raises it and reloads rather than stitching
     * two independently-solved lane graphs together.
     */
    loadGraph: async (
      repoPath: string,
      query = "",
      revision: string | null = null,
      opts: { forceLimit?: number } = {}
    ) => {
      if (!repoPath) return;
      const token = bump(repoPath);
      const max = opts.forceLimit ?? limitFor(repoPath);
      update((s) => {
        if (visiblePath === repoPath) {
          return { ...s, isLoading: true, error: null, visiblePath: repoPath };
        }
        return s;
      });
      try {
        const payload = await invokeFn<CommitGraphPayload>("cmd_get_commit_graph", {
          repoPath,
          maxCommits: max,
          query: query || null,
          revision: revision || null,
        });
        if (!isCurrent(repoPath, token)) return;

        const previous = cache.get(repoPath);
        const keepId = previous?.selectedCommit?.id;
        const kept = keepId ? payload.rows.find((row) => row.id === keepId) : undefined;
        const selectedCommit =
          kept ?? (payload.rows.length > 0 ? (payload.rows[0] ?? null) : null);
        const keepDetails =
          previous &&
          selectedCommit &&
          previous.selectedCommitDetails?.id === selectedCommit.id
            ? previous.selectedCommitDetails
            : null;
        const next: CachedGraph = {
          rows: payload.rows,
          commits: payload.rows,
          folds: payload.folds ?? [],
          refs: payload.refs ?? [],
          headId: payload.head_id,
          selectedCommit,
          selectedCommitDetails: keepDetails,
          query,
          revision,
          maxCommits: max,
          hasMore: payload.has_more === true,
        };
        cache.set(repoPath, next);
        if (visiblePath === repoPath) {
          update((s) => ({
            ...s,
            rows: next.rows,
            commits: next.commits,
            folds: next.folds,
            refs: next.refs,
            headId: next.headId,
            selectedCommit: next.selectedCommit,
            selectedCommitDetails: next.selectedCommitDetails,
            maxCommits: max,
            hasMore: next.hasMore,
            error: null,
            isLoading: false,
            visiblePath: repoPath,
          }));
        }

        if (selectedCommit) {
          await fetchDetails(repoPath, token, selectedCommit.id);
        }
      } catch (err: unknown) {
        if (!isCurrent(repoPath, token)) return;
        // A failed load is reported, not laundered into an empty graph: the
        // last good cache stays intact so switching tabs serves real history.
        if (visiblePath === repoPath) {
          update((s) => ({
            ...s,
            isLoading: false,
            hasMore: false,
            error: String(err),
            visiblePath: repoPath,
          }));
        }
      }
    },
    /** Raises the page ceiling for a repository and reloads. */
    loadMore: async (repoPath: string, query = "", revision: string | null = null) => {
      const current = limitFor(repoPath);
      const next = nextLoadLimit(current);
      if (next === null || !repoPath) return false;
      await api.loadGraph(repoPath, query, revision, { forceLimit: next });
      return true;
    },
    selectCommit: async (commit: VisualCommitRow, repoPath?: string) => {
      const path = repoPath ?? visiblePath;
      update((s) => ({ ...s, selectedCommit: commit }));
      if (path && visiblePath === path) {
        const cached = cache.get(path);
        if (cached) cached.selectedCommit = commit;
      }
      if (path && commit) {
        const token = generations.get(path) ?? 0;
        const seq = selectionSeq(path);
        await fetchDetails(path, token, commit.id, seq);
      }
    },
    applyClientFilter: (_query: string) => {
      // Filtering is applied by CommitTable against the latest backend payload.
    },
  };

  return api;
}

export type GraphStoreApi = ReturnType<typeof createGraphStore>;

export const graphStore = createGraphStore();
