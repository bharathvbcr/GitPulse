/**
 * Lane occupancy helpers for solved graph rows.
 *
 * The lane solver assigns each branch segment ONE column for its entire
 * lifetime (interval allocation over the segment's full visual span,
 * in-flight connectors included), and recycles a column only after its
 * previous occupant has fully ended. A solver lane index therefore IS the
 * final visual column: `x = originX + lane * laneWidth`, on every row,
 * forever.
 *
 * The old per-row display packing that used to live here — renaming the
 * lanes live on each row onto 0..k-1 — is deliberately gone. It existed to
 * hide the holes the old greedy allocator left, and it is what made every
 * lane right of a dying neighbour jog sideways (the stair-step artifacts)
 * and hit-testing drift off the drawn pixels. A transient hole between two
 * live branches is now honest, stable whitespace, exactly as GitKraken
 * draws it; total width still equals peak concurrent occupancy because the
 * solver's interval allocation is optimal.
 */

export interface LaneDisplayRow {
  lane?: number;
  active_lanes?: readonly number[] | null;
  connections?: readonly {
    from_lane?: number;
    to_lane?: number;
    is_dangling?: boolean;
  }[];
}

function isLiveLane(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

function addLive(used: Set<number>, value: unknown): void {
  if (isLiveLane(value)) used.add(value);
}

/**
 * Sorted unique lanes that occupy or leave this row — the node's own lane,
 * pass-throughs, and connector origins. Used by hit-testing to know which
 * columns are interactive on a row.
 */
export function collectLiveLanes(row: LaneDisplayRow): number[] {
  const used = new Set<number>();
  addLive(used, row.lane);
  if (row.active_lanes) {
    for (const lane of row.active_lanes) addLive(used, lane);
  }
  if (row.connections) {
    for (const conn of row.connections) {
      addLive(used, conn.from_lane);
    }
  }
  return [...used].sort((a, b) => a - b);
}

/**
 * Highest column index anything in these rows occupies: nodes,
 * pass-throughs, connector origins, and live connector destinations (an
 * edge needs its target column even when the parent row is far below).
 * Dangling to_lanes are ignored — stubs draw on from_lane.
 */
export function maxOccupiedLane(rows: readonly LaneDisplayRow[]): number {
  let max = 0;
  for (const row of rows) {
    if (isLiveLane(row.lane) && row.lane > max) max = row.lane;
    if (row.active_lanes) {
      for (const lane of row.active_lanes) {
        if (isLiveLane(lane) && lane > max) max = lane;
      }
    }
    if (row.connections) {
      for (const conn of row.connections) {
        if (isLiveLane(conn.from_lane) && conn.from_lane > max) max = conn.from_lane;
        if (!conn.is_dangling && isLiveLane(conn.to_lane) && conn.to_lane > max) {
          max = conn.to_lane;
        }
      }
    }
  }
  return max;
}

const liveCache = new WeakMap<object, number[][]>();

/** Per-row live sets, memoized on the payload array identity. */
export function liveLanesByRow(rows: readonly LaneDisplayRow[]): number[][] {
  const hit = liveCache.get(rows as object);
  if (hit && hit.length === rows.length) return hit;
  const lives = rows.map(collectLiveLanes);
  liveCache.set(rows as object, lives);
  return lives;
}
