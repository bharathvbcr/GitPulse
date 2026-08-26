import { writable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { formatError } from "../ui/formatError";
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
  /**
   * Non-fatal reads that failed backend-side (HEAD, ref decorations).
   * Empty means every read ran — not that the repo has no decorations.
   */
  warnings?: string[];
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
  /** Backend reads that failed this load (HEAD, ref decorations). */
  warnings: string[];
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
  warnings: string[];
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
    warnings: [],
  };
}

export function createGraphStore(deps: { invoke?: InvokeFn } = {}) {
  const invokeFn = deps.invoke ?? (invoke as InvokeFn);
  const cache = new Map<string, CachedGraph>();
  /**
   * Ordering epochs come from store-wide monotonic counters, so a value is
   * never handed out twice. That is what makes pruning safe: evict() deletes
   * both maps' entries for the path (absent entry ⇒ every outstanding token
   * for it is dead), and a fresh load can never collide with an orphaned
   * fetch the way a per-path counter reset to 1 would.
   */
  const generations = new Map<string, number>();
  /** Monotonic per-repo ordering so a slow details fetch from an older
   * selection can never overwrite a newer one's pane. */
  const selections = new Map<string, number>();
  let epochSource = 0;
  let selectionSource = 0;
  let visiblePath: string | null = null;

  const { subscribe, update, set } = writable<GraphState>(
    emptyVisible(null, DEFAULT_MAX_COMMITS, false)
  );

  function bump(path: string): number {
    const next = ++epochSource;
    generations.set(path, next);
    return next;
  }

  function isCurrent(path: string, token: number): boolean {
    return generations.get(path) === token;
  }

  function selectionSeq(path: string): number {
    const next = ++selectionSource;
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
          warnings: cached.warnings,
          error: null,
          isLoading: false,
          visiblePath: path,
        }));
        return;
      }
      update(() => emptyVisible(path, limitFor(path), true));
    },
    evict: (path: string) => {
      cache.delete(path);
      // Prune the ordering maps too: entries for closed repos would otherwise
      // grow without bound. A missing entry orphans every outstanding token
      // for this path (undefined === token is false), which is exactly what
      // the old bump achieved; uniqueness of fresh tokens comes from the
      // store-wide sources above.
      generations.delete(path);
      selections.delete(path);
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
          warnings: payload.warnings ?? [],
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
            warnings: next.warnings,
            error: null,
            isLoading: false,
            visiblePath: repoPath,
          }));
        }
        // Backend reads that failed land in the diagnostics channel; a graph
        // without decorations must never be silently indistinguishable from
        // a repository that has none.
        for (const warning of next.warnings) {
          console.warn(`[graph:${repoPath}] ${warning}`);
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
            error: formatError(err),
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
