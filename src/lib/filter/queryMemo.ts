import { matchesCommit, parseFilterQuery, type ParsedFilterQuery } from "./parseQuery";

/**
 * Parse-once memo for filter queries.
 *
 * CommitTable re-ran `parseFilterQuery` on every render of its filtered
 * derivation; typing "fix:" produced a fresh parse per keystroke per row pass.
 * Raw strings repeat heavily (every render between keystrokes), so a small LRU
 * keyed on the raw string removes the repeated work without changing results.
 * Returned objects are frozen and shared — callers must treat them as
 * immutable.
 */
export const PARSE_MEMO_CAP = 32;

export interface CachedQueryParser {
  parse(raw: string): ParsedFilterQuery;
  readonly size: number;
  readonly misses: number;
}

export function createCachedQueryParser(cap: number = PARSE_MEMO_CAP): CachedQueryParser {
  const cache = new Map<string, ParsedFilterQuery>();
  let misses = 0;
  return {
    parse(raw: string): ParsedFilterQuery {
      const hit = cache.get(raw);
      if (hit) {
        // Re-insert to refresh recency: Map iteration is insertion-ordered,
        // so the oldest key is always first and eviction below is LRU.
        cache.delete(raw);
        cache.set(raw, hit);
        return hit;
      }
      misses += 1;
      const parsed = Object.freeze(parseFilterQuery(raw));
      cache.set(raw, parsed);
      if (cache.size > cap) {
        const oldest = cache.keys().next();
        if (!oldest.done) cache.delete(oldest.value);
      }
      return parsed;
    },
    get size() {
      return cache.size;
    },
    get misses() {
      return misses;
    },
  };
}

export const queryParser = createCachedQueryParser();

export function parseFilterQueryCached(raw: string): ParsedFilterQuery {
  return queryParser.parse(raw);
}

/** Structural subset both filters and lane accounting need from a row. */
export interface FilterableRow {
  id: string;
  summary: string;
  author_name: string;
  author_email: string;
  lane?: number;
  active_lanes?: readonly number[] | null;
  /** Present on solved graph rows; required for connection remapping below. */
  parent_ids?: readonly string[];
  connections?: unknown[];
}

export interface FilteredRows<R> {
  rows: R[];
  maxActiveLane: number;
}

/**
 * Rebuilds a row's `to_row_offset` values against the FILTERED array.
 *
 * The lane solver bakes array-index arithmetic into every connection:
 * `to_row_offset` is parent_index - child_index against the array IT solved.
 * Filtering removes rows out of the middle, so every offset pointing past a
 * removed row now lands on whatever unrelated commit shifted into the slot —
 * edges drawn between commits that have no relation at all, plus edges whose
 * recomputed endpoint fell off the end and were silently dropped.
 *
 * The fix uses the invariant connection k ↔ parent_ids[k] (the solver emits
 * exactly one connection per parent, in order): the parent's NEW index is a
 * map lookup, and the new offset is the new-index gap, which shrinks by
 * exactly the number of removed rows between child and parent. A parent that
 * was filtered out flips the edge to `is_dangling` so it renders as an
 * honest stub instead of a line into a stranger. Lane geometry (lanes,
 * colors, active-lane snapshots) is per-row solver state and never shifts.
 */
function remapConnections<R extends FilterableRow>(
  row: R,
  newRowIdx: number,
  newIndexById: Map<string, number>,
): R | null {
  const conns = row.connections;
  const parents = row.parent_ids;
  if (!conns || conns.length === 0) return null;
  let changed = false;
  const next = conns.map((conn, k) => {
    const typed = conn as {
      to_row_offset: number;
      is_dangling?: boolean;
      [key: string]: unknown;
    };
    if (typed.is_dangling) return conn;
    const parentId =
      parents && typeof parents[k] === "string" ? (parents[k] as string) : undefined;
    // Fail closed: without a parent id there is no honest endpoint left.
    const parentNew = parentId !== undefined ? newIndexById.get(parentId) : undefined;
    if (parentNew === undefined || parentNew <= newRowIdx) {
      changed = true;
      return { ...typed, to_row_offset: 1, is_dangling: true };
    }
    const updated = parentNew - newRowIdx;
    if (updated === typed.to_row_offset) return conn;
    changed = true;
    return { ...typed, to_row_offset: updated };
  });
  if (!changed) return null;
  return { ...row, connections: next };
}

/**
 * Filters rows and finds the widest active lane in ONE pass.
 *
 * When any row is removed and the rows carry solved connections, survivor
 * connections are remapped onto the filtered coordinates (see
 * {@link remapConnections}). When NOTHING was filtered out the input rows are
 * returned as-is — identity preserved, offsets already correct, zero copies.
 */
export function filterRowsWithLanes<R extends FilterableRow>(
  rows: readonly R[],
  parsed: ParsedFilterQuery,
): FilteredRows<R> {
  let maxActiveLane = 0;
  const kept: R[] = [];
  const keptOriginalIdx: number[] = [];
  for (let i = 0; i < rows.length; i++) {
    const row = rows[i];
    if (!matchesCommit(row, parsed)) continue;
    kept.push(row);
    keptOriginalIdx.push(i);
    if (row.lane !== undefined && row.lane > maxActiveLane) maxActiveLane = row.lane;
    if (!row.active_lanes) continue;
    for (const lane of row.active_lanes) {
      if (lane > maxActiveLane) maxActiveLane = lane;
    }
  }
  if (kept.length === rows.length) {
    // Nothing filtered out: hand back the caller's own array so downstream
    // identity checks (canvas cache versioning) see zero change.
    return { rows: rows as R[], maxActiveLane };
  }

  const newIndexById = new Map<string, number>();
  for (let i = 0; i < kept.length; i++) newIndexById.set(kept[i].id, i);

  let mutated = false;
  const remapped: R[] = new Array(kept.length);
  for (let i = 0; i < kept.length; i++) {
    const rebuilt = remapConnections(kept[i], i, newIndexById);
    remapped[i] = rebuilt ?? kept[i];
    if (rebuilt !== null) mutated = true;
  }
  // No connection pointed past a removed row: keep original objects so the
  // strip cache's identity checks see "unchanged geometry".
  return { rows: mutated ? remapped : kept, maxActiveLane };
}

/**
 * Queries memoized per rows identity before eviction. Far more than any UI
 * burst produces between row-array replacements, and small enough that a
 * pathological keystroke sequence cannot grow the map without bound.
 */
export const ROW_FILTER_MEMO_CAP = PARSE_MEMO_CAP;

export interface RowFilterMemo {
  filter<R extends FilterableRow>(rows: readonly R[], parsed: ParsedFilterQuery): FilteredRows<R>;
}

/**
 * Structural key for a parsed query. Callers pass either shared frozen objects
 * from {@link queryParser} or freshly parsed ones with equal content; keying on
 * object identity alone would recompute for every fresh parse and defeat the
 * memo exactly where typing bursts need it most.
 */
function parsedQuerySignature(parsed: ParsedFilterQuery): string {
  // \u0000 separators cannot occur inside the matched values (author/sha/type
  // are token slices of whitespace-split input), so keys cannot collide.
  return `${parsed.author ?? ""}\u0000${parsed.path ?? ""}\u0000${parsed.sha ?? ""}\u0000${parsed.commitType ?? ""}\u0000${parsed.text}`;
}

/**
 * Identity-stable wrapper around filterRowsWithLanes.
 *
 * filterRowsWithLanes allocates a fresh result per call; CommitTable derives
 * from it and bumps its canvas dataVersion on every new ARRAY IDENTITY, so a
 * store emission that reuses the same rows reference must not invalidate the
 * strip cache. Results are cached while the rows reference holds and keyed by
 * the parsed query's CONTENT (fresh-but-equal parses share one entry); a new
 * rows array invalidates every cached result, because row content may have
 * changed even when shape did not. Eviction is LRU over the query signatures,
 * so an evicted query recomputes correctly instead of returning stale data.
 */
export function createRowFilterMemo(cap: number = ROW_FILTER_MEMO_CAP): RowFilterMemo {
  let rowsRef: readonly FilterableRow[] | null = null;
  const byQuery = new Map<string, FilteredRows<FilterableRow>>();

  return {
    filter<R extends FilterableRow>(rows: readonly R[], parsed: ParsedFilterQuery): FilteredRows<R> {
      if (rows !== rowsRef) {
        rowsRef = rows;
        byQuery.clear();
      }
      const key = parsedQuerySignature(parsed);
      const hit = byQuery.get(key);
      if (hit) {
        // Re-insert to refresh recency: Map iteration is insertion-ordered,
        // so eviction below always drops the least recently used query.
        byQuery.delete(key);
        byQuery.set(key, hit);
        // Sound narrowing: the rows-identity check above guarantees this
        // entry was computed from exactly this `rows` array, so it IS a
        // FilteredRows<R>; the shared map merely erases the generic.
        return hit as FilteredRows<R>;
      }
      const fresh = filterRowsWithLanes(rows, parsed);
      byQuery.set(key, fresh as FilteredRows<FilterableRow>);
      if (byQuery.size > cap) {
        const oldest = byQuery.keys().next();
        if (!oldest.done) byQuery.delete(oldest.value);
      }
      return fresh;
    },
  };
}
