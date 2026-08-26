import { writable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { formatError } from "../ui/formatError";
import { diagnostics, type DiagnosticsStore } from "../diagnostics/diagnostics";
import type { VisualCommitRow, LaneConnection } from "../canvas/GraphRenderer";
import {
  DEFAULT_MAX_COMMITS,
  nextLoadLimit,
} from "./graphLimits";
import { queryNeedsServerFetch } from "../graph/graphFetchScheduler";

export type InvokeFn = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * Sanitizes a filter query for a direct cmd_get_commit_graph reload. The
 * backend applies ANY query server-side when invoked, but the fetch scheduler
 * treats non-path queries as client-side filtering (graphRequestKey ignores
 * them). Forwarding one launders filtered rows into the cached payload: the
 * rows stay missing after the user clears the filter, because the scheduler's
 * key then equals its last-fired key and nothing refetches. Only `path:`
 * filters genuinely need git to walk history, so everything else is blanked.
 */
export function serverFetchableQuery(query: string): string {
  return queryNeedsServerFetch(query) ? query : "";
}

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

/** Structural fingerprint of one rendered lane connection. */
function laneConnectionSignature(c: LaneConnection): string {
  return `${c.from_lane}>${c.to_lane}@${c.to_row_offset}${c.is_merge ? "M" : ""}${c.is_dangling ? "d" : ""}:${c.color_index}`;
}

/** Structural fingerprint of one rendered row; field order is fixed, so two
 * structurally equal rows always serialize identically regardless of the key
 * order the producing backend or spread happened to use. Fields beyond
 * id/summary/parent_ids may be absent on legacy/partial payloads — they are
 * normalized so absence and emptiness hash the same instead of throwing. */
function rowSignature(row: SignatureRow): string {
  return [
    row.id,
    (row.parent_ids ?? []).join(","),
    row.summary ?? "",
    row.author_name ?? "",
    row.author_email ?? "",
    String(row.timestamp ?? 0),
    String(row.lane ?? -1),
    String(row.color_index ?? -1),
    (row.active_lanes ?? []).join(","),
    (row.active_lane_colors ?? []).join(","),
    (row.connections ?? []).map(laneConnectionSignature).join(";"),
    row.is_merge ? "1" : "0",
    row.is_root ? "1" : "0",
  ].join("|");
}

/**
 * What the signature actually reads from a row. Deliberately looser than
 * {@link VisualCommitRow}: partially-populated payloads (older backend
 * responses, tests) may omit render-only fields, and the serializer normalizes
 * absence instead of throwing. Every full VisualCommitRow is still assignable.
 */
type SignatureRow = Partial<Omit<VisualCommitRow, "id">> & Pick<VisualCommitRow, "id">;

/**
 * Structural equality hash over everything the graph pane renders.
 *
 * Background reloads (watcher refreshes, tab re-activations) routinely return
 * payloads that are byte-equal in content but fresh in identity; republishing
 * them would churn every subscriber and invalidate downstream caches keyed on
 * array identity. This signature lets loadGraph detect "nothing actually
 * changed" and skip the emission entirely. Deliberately string-concatenation
 * rather than a crypto hash: inputs are small, deterministic, and the codebase
 * keys caches this way throughout (themeSignatureOf, BranchList signatures).
 */
export function graphPayloadSignature(payload: {
  rows: readonly SignatureRow[];
  folds?: readonly FoldedBranchRun[] | null;
  head_id: string | null;
  refs?: readonly RefDecoration[] | null;
  has_more?: boolean;
}): string {
  const folds = (payload.folds ?? [])
    .map(
      (f) =>
        `${f.merge_commit_id}>${f.branch_root_id}#${f.folded_commit_ids.join(",")}@${f.commit_count}${f.is_collapsed ? "c" : "o"}`,
    )
    .join(";");
  const refs = (payload.refs ?? [])
    .map((r) => `${r.name}#${r.kind}@${r.commit_id}${r.is_head ? "*" : ""}`)
    .join(";");
  const rows = payload.rows.map(rowSignature).join(";");
  return `${payload.head_id ?? ""}\u0001${payload.has_more === true ? "more" : "end"}\u0001${folds}\u0001${refs}\u0001${rows}`;
}

/**
 * One comparable string for "did the rendered history change". A watcher
 * refresh re-walks history every few seconds; on a quiet repo the payload is
 * structurally identical (fresh identities, same content) and must not
 * republish — every emission wipes the canvas strip cache and re-renders the
 * table. Delegates to {@link graphPayloadSignature} so both call sites share
 * one canonical definition of "structurally identical payload".
 */
function cachedSignature(cache: {
  rows: VisualCommitRow[];
  folds: FoldedBranchRun[];
  headId: string | null;
  refs: RefDecoration[];
  hasMore: boolean;
}): string {
  return graphPayloadSignature({
    rows: cache.rows,
    folds: cache.folds,
    head_id: cache.headId,
    refs: cache.refs,
    has_more: cache.hasMore,
  });
}

export function createGraphStore(deps: { invoke?: InvokeFn; diagnostics?: Pick<DiagnosticsStore, "warn"> } = {}) {
  const invokeFn = deps.invoke ?? (invoke as InvokeFn);
  // Injectable so tests can observe breadcrumbs; the app-wide ring is the
  // default sink, same pattern as the invoke seam above.
  const reportFailure =
    deps.diagnostics?.warn ?? ((source: string, detail: unknown) => diagnostics.warn(source, detail));
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
  /**
   * Mirror of the current state for guards that must decide whether an
   * update is worth emitting — writable notifies on EVERY set, so a
   * no-op update still re-runs each subscriber effect.
   */
  let latestState: GraphState | null = null;
  subscribe((value) => {
    latestState = value;
  });

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
      // Stale-while-revalidate: when cached rows are already presented, a
      // background reload must not flash the progress bar over them. Only a
      // blank slate (first load, no cache) shows the loading state.
      if (!cache.get(repoPath) && visiblePath === repoPath) {
        update((s) => ({ ...s, isLoading: true, error: null, visiblePath: repoPath }));
      }
      try {
        const payload = await invokeFn<CommitGraphPayload>("cmd_get_commit_graph", {
          repoPath,
          maxCommits: max,
          query: query || null,
          revision: revision || null,
        });
        if (!isCurrent(repoPath, token)) return;

        const previous = cache.get(repoPath);
        if (
          previous &&
          previous.query === query &&
          previous.revision === revision &&
          previous.maxCommits === max &&
          cachedSignature(previous) === graphPayloadSignature(payload)
        ) {
          // Identical reload: retire the flag, keep every array identity so
          // downstream caches (canvas strips) survive untouched.
          if (visiblePath === repoPath && latestState?.isLoading) {
            update((s) => ({ ...s, isLoading: false }));
          }
          return;
        }

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
        // A background load has no pane to raise an error banner, so leave a
        // diagnostics breadcrumb — an invisible failure used to vanish with
        // no trace at all. Visible failures keep their banner as the one
        // breadcrumb; double-reporting them would only duplicate it.
        if (visiblePath !== repoPath) {
          reportFailure("graph-load", `graph load failed for ${repoPath}: ${formatError(err)}`);
        }
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
