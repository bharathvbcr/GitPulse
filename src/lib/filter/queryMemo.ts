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
}

export interface FilteredRows<R> {
  rows: R[];
  maxActiveLane: number;
}

/**
 * Filters rows and finds the widest active lane in ONE pass.
 *
 * The old pair of derivations walked the rows twice per render (filter, then a
 * separate max-lane scan over the survivors). Lane values that are NaN or
 * missing never widen the result, matching the previous `>` comparisons.
 */
export function filterRowsWithLanes<R extends FilterableRow>(
  rows: readonly R[],
  parsed: ParsedFilterQuery,
): FilteredRows<R> {
  let maxActiveLane = 0;
  const kept: R[] = [];
  for (const row of rows) {
    if (!matchesCommit(row, parsed)) continue;
    kept.push(row);
    if (row.lane !== undefined && row.lane > maxActiveLane) maxActiveLane = row.lane;
    if (!row.active_lanes) continue;
    for (const lane of row.active_lanes) {
      if (lane > maxActiveLane) maxActiveLane = lane;
    }
  }
  return { rows: kept, maxActiveLane };
}
