import type { LaneConnection, VisualCommitRow } from "./GraphRenderer";

/**
 * Incoming-edge index over solved graph rows.
 *
 * Connectors are owned by their CHILD row: the child carries
 * `to_row_offset`, the distance down to its parent. Rendering "every edge
 * touching the viewport" therefore means finding the children ABOVE the
 * window whose parents landed inside it — a query the child-side arrays
 * cannot answer without scanning an unbounded band above the view.
 *
 * This index inverts the relation once per row-array identity: for each row
 * `t`, the ordered list of child indices whose connections target `t`.
 * Building costs O(rows + edges) time and flat typed-array memory (same
 * order as the payload itself); queries are O(edges-found), never O(history).
 *
 * Memoized on array identity through a WeakMap: payloads are immutable
 * snapshots, so identity is a sound cache key, and dropping the array drops
 * the index with it.
 */
export interface IncomingEdgeIndex {
  /** Row count the index was built from. */
  readonly rowCount: number;
  /**
   * For target row `t`: children are
   * `children[starts[t] .. starts[t+1]]` (ascending, duplicates preserved —
   * a commit listing the same parent twice draws both edges).
   */
  readonly starts: Int32Array;
  readonly children: Int32Array;
}

const indexCache = new WeakMap<VisualCommitRow[], IncomingEdgeIndex>();

/** Parent row for a child-owned connector, or null when the offset is not a live forward edge. */
export function connectionTargetIndex(
  fromIndex: number,
  offset: number,
  rowCount: number,
): number | null {
  if (!Number.isFinite(offset)) return null;
  const target = fromIndex + offset;
  if (!(target > fromIndex && target < rowCount)) return null;
  return target;
}

export function buildIncomingEdgeIndex(rows: VisualCommitRow[]): IncomingEdgeIndex {
  const cached = indexCache.get(rows);
  if (cached) return cached;

  const n = rows.length;
  // Pass 1: count incoming edges per target so the CSR arrays are exact-sized.
  const counts = new Int32Array(n);
  for (let i = 0; i < n; i++) {
    const conns = rows[i].connections;
    if (!conns) continue;
    for (let k = 0; k < conns.length; k++) {
      const target = connectionTargetIndex(i, conns[k].to_row_offset, n);
      if (target !== null) counts[target] += 1;
    }
  }

  const starts = new Int32Array(n + 1);
  for (let t = 0; t < n; t++) starts[t + 1] = starts[t] + counts[t];
  const children = new Int32Array(starts[n]);

  // Pass 2: place child indices. Per-target cursors keep the fill O(E).
  const cursor = starts.slice(0, n);
  for (let i = 0; i < n; i++) {
    const conns = rows[i].connections;
    if (!conns) continue;
    for (let k = 0; k < conns.length; k++) {
      const target = connectionTargetIndex(i, conns[k].to_row_offset, n);
      if (target !== null) children[cursor[target]++] = i;
    }
  }

  const index: IncomingEdgeIndex = { rowCount: n, starts, children };
  indexCache.set(rows, index);
  return index;
}

/**
 * The child row closest above `fromRow` whose connection lands on a target
 * in `[fromRow, toRow)` — i.e. how far up rendering must reach so every
 * edge touching the window is started from its own child. Returns `fromRow`
 * when no incoming edge reaches the window (nothing to extend toward).
 */
export function deepestChildTargetingRange(
  index: IncomingEdgeIndex,
  fromRow: number,
  toRow: number,
): number {
  const lo = Math.max(0, Math.min(fromRow, index.rowCount));
  const hi = Math.min(Math.max(toRow, lo), index.rowCount);
  let best = fromRow;
  for (let t = lo; t < hi; t++) {
    for (let p = index.starts[t]; p < index.starts[t + 1]; p++) {
      const child = index.children[p];
      if (child < best) best = child;
    }
  }
  return best;
}

/**
 * True when a connector's span exceeds the lookback the strip-cache tiles
 * prime with. Long connectors cannot be baked into tiles: the tile showing
 * the parent would need to prime back past the canvas-height budget, so the
 * middle of the edge would be drawn by nobody and appear as a floating
 * fragment at a seam. They are drawn whole by the live overlay instead —
 * see {@link GraphRenderer.drawLongConnectors}. Single owner of the
 * threshold so the two layers can never disagree about who owns an edge.
 */
export function isLongConnection(
  conn: Pick<LaneConnection, "to_row_offset">,
  lookbackRows: number,
): boolean {
  return conn.to_row_offset > lookbackRows;
}
