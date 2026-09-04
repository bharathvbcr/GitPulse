import { get, writable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { formatError } from "../ui/formatError";
import { diagnostics, type DiagnosticsStore } from "../diagnostics/diagnostics";
import type { VisualCommitRow, LaneConnection } from "../canvas/GraphRenderer";
import {
  DEFAULT_MAX_COMMITS,
  nextLoadLimit,
} from "./graphLimits";
import { normalizeGraphQuery } from "../graph/graphFetchScheduler";
import { isRefScope, type RefScope } from "../graph/refScope";
import { interfaceStore } from "./interfaceStore";

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

/** A branch, remote branch, tag or detached HEAD pointing at a row. */
/**
 * Mirrors Rust's `RefKind` enum, which carries
 * `#[serde(rename_all = "lowercase")]`. Named rather than inlined so the
 * enum-variant contract can compare the two sides: a renamed variant would
 * otherwise leave TypeScript compiling while the comparison stopped matching.
 */
export type RefKind = "local" | "remote" | "tag" | "head" | "other";

/**
 * Re-exported, not redeclared. A second copy of the union here would be a
 * third place the scope is written down, and the whole point of
 * `graph/refScope.ts` is that there is one.
 */
export type { RefScope } from "../graph/refScope";

export interface RefDecoration {
  name: string;
  kind: RefKind;
  commit_id: string;
  is_head: boolean;
}

export interface CommitGraphPayload {
  rows: VisualCommitRow[];
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
   * Degradations the backend hit while assembling this page (HEAD resolution
   * or ref decoration listing failing without failing the load). Optional:
   * payloads from before the field existed omit it.
   */
  warnings?: string[];
  /**
   * The commit at the top of the pinned mainline — the straight column-0
   * rail the solver keeps for the default branch. Absent on payloads from
   * before the field existed; null only when the graph has no rows.
   */
  mainline_id?: string | null;
  /**
   * The branch the mainline was anchored on (`main`, `origin/main`, the
   * HEAD branch), or null when the newest commit's chain was pinned because
   * no ref resolved.
   */
  mainline_name?: string | null;
}

export interface GraphState {
  rows: VisualCommitRow[];
  commits: VisualCommitRow[];
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
  /** Top of the straight mainline column; null when there are no rows. */
  mainlineId: string | null;
  /** Ref the mainline is anchored on, for labelling; null when unnamed. */
  mainlineName: string | null;
}

interface CachedGraph {
  rows: VisualCommitRow[];
  commits: VisualCommitRow[];
  refs: RefDecoration[];
  headId: string | null;
  selectedCommit: VisualCommitRow | null;
  selectedCommitDetails: CommitDetails | null;
  query: string;
  revision: string | null;
  /**
   * The ref scope this page was walked under. Part of the cache key: the
   * same repository at the same query answers with a different set of rows
   * under `all`, so a scope change must invalidate rather than reuse.
   */
  refScope: RefScope;
  maxCommits: number;
  hasMore: boolean;
  warnings: string[];
  mainlineId: string | null;
  mainlineName: string | null;
}
function emptyVisible(
  path: string | null,
  maxCommits: number,
  loading: boolean
): GraphState {
  return {
    rows: [],
    commits: [],
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
    mainlineId: null,
    mainlineName: null,
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
    row.is_mainline ? "1" : "0",
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
  head_id: string | null;
  refs?: readonly RefDecoration[] | null;
  has_more?: boolean;
  mainline_id?: string | null;
  mainline_name?: string | null;
}): string {
  const refs = (payload.refs ?? [])
    .map((r) => `${r.name}#${r.kind}@${r.commit_id}${r.is_head ? "*" : ""}`)
    .join(";");
  const rows = payload.rows.map(rowSignature).join(";");
  const mainline = `${payload.mainline_id ?? ""}#${payload.mainline_name ?? ""}`;
  return `${payload.head_id ?? ""}\u0001${payload.has_more === true ? "more" : "end"}\u0001${refs}\u0001${mainline}\u0001${rows}`;
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
  headId: string | null;
  refs: RefDecoration[];
  hasMore: boolean;
  mainlineId: string | null;
  mainlineName: string | null;
}): string {
  return graphPayloadSignature({
    rows: cache.rows,
    head_id: cache.headId,
    refs: cache.refs,
    has_more: cache.hasMore,
    mainline_id: cache.mainlineId,
    mainline_name: cache.mainlineName,
  });
}

export function createGraphStore(
  deps: {
    invoke?: InvokeFn;
    diagnostics?: Pick<DiagnosticsStore, "warn">;
    /**
     * The ref scope every load asks for. Read here rather than threaded
     * through `loadGraph`'s six call sites: a caller that forgot the argument
     * would silently reload the graph under a different scope than the one on
     * screen — the same failure mode as the bare `loadGraph(path)` that used
     * to reset query and branch.
     */
    refScope?: () => RefScope;
  } = {}
) {
  const invokeFn = deps.invoke ?? (invoke as InvokeFn);
  // Normalized at the seam: a persisted preference is user data, and the IPC
  // boundary must never receive a value the backend will refuse to
  // deserialize. An unrecognized scope degrades to the named set rather than
  // failing the whole graph load.
  const readRefScope = deps.refScope ?? (() => get(interfaceStore).graphRefScope);
  const refScopeOf = (override?: RefScope): RefScope => {
    if (isRefScope(override)) return override;
    const stored = readRefScope();
    return isRefScope(stored) ? stored : "named";
  };
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
  /**
   * The warning set last written to diagnostics, per repository.
   *
   * Assembly warnings are STATE, not events: a repository with refs outside
   * the walked scope reports the same sentence on every load, and the watcher
   * reloads on every settled write. The diagnostics ring coalesces identical
   * CONSECUTIVE entries, which handles one persistent warning but not two —
   * they alternate, each displacing the other as newest, and a repository with
   * both fills the ring with the same pair forever, burying everything else.
   * The live set still rides in `state.warnings`; this only stops the log from
   * repeating what has not changed.
   */
  const warnedSignatures = new Map<string, string>();
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
    } catch (err) {
      // Details are best-effort: the row itself already renders, so a
      // failure only costs the lower pane's content — but it must still
      // leave a breadcrumb instead of looking like an eternal blank.
      reportFailure("graph-details", `${repoPath}/${commitId}: ${formatError(err)}`);
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
          refs: cached.refs,
          headId: cached.headId,
          selectedCommit: cached.selectedCommit,
          selectedCommitDetails: cached.selectedCommitDetails,
          maxCommits: cached.maxCommits,
          hasMore: cached.hasMore,
          warnings: cached.warnings,
          mainlineId: cached.mainlineId,
          mainlineName: cached.mainlineName,
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
      warnedSignatures.delete(path);
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
      opts: { forceLimit?: number; refScope?: RefScope } = {}
    ) => {
      if (!repoPath) return;
      query = normalizeGraphQuery(query);
      const token = bump(repoPath);
      const max = opts.forceLimit ?? limitFor(repoPath);
      // Stale-while-revalidate: a background reload of the SAME view must
      // not flash the progress bar over the cached rows. A blank slate, or
      // rows that answer a different query or branch than the one asked
      // for, shows the loading state: the user just typed a filter and
      // must see it working, not a silent row swap half a second later.
      const refScope = refScopeOf(opts.refScope);
      const cached = cache.get(repoPath);
      const refining =
        cached !== undefined &&
        (cached.query !== query ||
          cached.revision !== revision ||
          cached.refScope !== refScope);
      if ((!cached || refining) && visiblePath === repoPath) {
        update((s) => ({ ...s, isLoading: true, error: null, visiblePath: repoPath }));
      }
      try {
        const payload = await invokeFn<CommitGraphPayload>("cmd_get_commit_graph", {
          repoPath,
          maxCommits: max,
          query: query || null,
          revision: revision || null,
          refScope,
        });
        if (!isCurrent(repoPath, token)) return;

        // Assembly warnings ride along with an otherwise-good payload: each
        // one marks a degraded facet (HEAD marker, ref labels, history the
        // walked scope leaves out) that would otherwise render as if it were
        // honestly empty.
        //
        // Handled BEFORE the identical-payload short-circuit below. Warnings
        // are not part of the rendered-history signature, so a degradation
        // that appears while the rows stay the same — a ref listing that
        // starts failing, a namespace that starts hiding commits — returned
        // early and was never reported at all. Whether history changed and
        // whether the load degraded are two different questions.
        //
        // Logged when the SET changes: a warning that is still true is not
        // news, the watcher reloads on every settled write, and re-logging
        // the same pair every few seconds is how a ring buffer loses the
        // entry that mattered. The live set still rides in `state.warnings`.
        const warningList = payload.warnings ?? [];
        const warningSignature = warningList.join("\u0001");
        if (warningSignature === "") {
          warnedSignatures.delete(repoPath);
        } else if (warnedSignatures.get(repoPath) !== warningSignature) {
          warnedSignatures.set(repoPath, warningSignature);
          for (const w of warningList) {
            reportFailure("graph", w);
          }
        }

        const previous = cache.get(repoPath);
        if (
          previous &&
          previous.query === query &&
          previous.revision === revision &&
          previous.refScope === refScope &&
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
          refs: payload.refs ?? [],
          headId: payload.head_id,
          selectedCommit,
          selectedCommitDetails: keepDetails,
          query,
          revision,
          refScope,
          maxCommits: max,
          hasMore: payload.has_more === true,
          warnings: payload.warnings ?? [],
          mainlineId: payload.mainline_id ?? null,
          mainlineName: payload.mainline_name ?? null,
        };
        cache.set(repoPath, next);
        if (visiblePath === repoPath) {
          update((s) => ({
            ...s,
            rows: next.rows,
            commits: next.commits,
            refs: next.refs,
            headId: next.headId,
            selectedCommit: next.selectedCommit,
            selectedCommitDetails: next.selectedCommitDetails,
            maxCommits: max,
            hasMore: next.hasMore,
            warnings: next.warnings,
            mainlineId: next.mainlineId,
            mainlineName: next.mainlineName,
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
  };

  return api;
}

export type GraphStoreApi = ReturnType<typeof createGraphStore>;

export const graphStore = createGraphStore();
