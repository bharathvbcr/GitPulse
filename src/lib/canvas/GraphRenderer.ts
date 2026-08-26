import { clamp01 } from "../motion/easing";
import { getBranchColor } from "./Palette";
import { authorColor, authorIdentity } from "../authors/authorIdentity";
import {
  buildIncomingEdgeIndex,
  collectLongEdges,
  deepestChildTargetingRange,
  isLongConnection,
  connectionTargetIndex,
} from "./graphEdges";
import { collectLiveLanes, liveLanesByRow, maxOccupiedLane } from "./laneDisplay";

export interface LaneConnection {
  from_lane: number;
  to_lane: number;
  to_row_offset: number;
  is_merge: boolean;
  color_index: number;
  /** True when the parent is outside the loaded window: the edge leaves the graph. */
  is_dangling?: boolean;
}

export interface VisualCommitRow {
  id: string;
  parent_ids: string[];
  summary: string;
  author_name: string;
  author_email: string;
  timestamp: number;
  lane: number;
  color_index: number;
  active_lanes: number[];
  active_lane_colors: number[];
  connections: LaneConnection[];
  is_merge: boolean;
  is_root: boolean;
}

export interface GraphRenderConfig {
  rowHeight: number;
  laneWidth: number;
  nodeRadius: number;
  mergeNodeRadius: number;
  lineWidth: number;
  originX: number;
}

/**
 * Colours the graph borrows from the application theme.
 *
 * They are passed in rather than hard-coded because the node cutout has to be
 * the exact colour of the surface behind it — a fixed dark value paints black
 * discs over a light theme.
 */
export interface GraphTheme {
  /** The colour behind the graph; used for the cutout under each node. */
  background: string;
  /** Thin outline that separates a node from its own lane. */
  nodeStroke: string;
  /** Ring drawn around the selected commit. */
  selection: string;
  /** Ring drawn around the commit HEAD points at. */
  head: string;
  /** Hover feedback and de-emphasised marks. */
  muted: string;
}

export type DensityMode = "spacious" | "compact";

export const DENSITY_CONFIGS: Record<DensityMode, GraphRenderConfig> = {
  spacious: {
    rowHeight: 36,
    laneWidth: 30,
    nodeRadius: 5,
    mergeNodeRadius: 6.5,
    lineWidth: 2.5,
    originX: 20,
  },
  compact: {
    rowHeight: 26,
    laneWidth: 18,
    nodeRadius: 3.5,
    mergeNodeRadius: 4.5,
    lineWidth: 2,
    originX: 14,
  },
};

export const DEFAULT_CONFIG: GraphRenderConfig = DENSITY_CONFIGS.spacious;

export const DEFAULT_THEME: GraphTheme = {
  background: "#0d1117",
  nodeStroke: "#161b22",
  selection: "#58a6ff",
  head: "#f0f6fc",
  muted: "#8b949e",
};

/**
 * Reads the theme out of the stylesheet, so the graph follows the light/dark
 * switch without a second source of truth for the palette.
 */
export function themeFromCss(element?: Element | null): GraphTheme {
  if (typeof window === "undefined" || typeof getComputedStyle !== "function") {
    return { ...DEFAULT_THEME };
  }
  const styles = getComputedStyle(element ?? document.documentElement);
  const read = (name: string, fallback: string) => {
    const value = styles.getPropertyValue(name).trim();
    return value.length > 0 ? value : fallback;
  };
  return {
    background: read("--bg-main", DEFAULT_THEME.background),
    nodeStroke: read("--bg-surface", DEFAULT_THEME.nodeStroke),
    selection: read("--accent-color", DEFAULT_THEME.selection),
    head: read("--text-primary", DEFAULT_THEME.head),
    muted: read("--text-muted", DEFAULT_THEME.muted),
  };
}

export interface RenderOptions {
  theme?: GraphTheme;
  /** Commit under the pointer, highlighted with a soft ring. */
  hoveredCommitId?: string | null;
  /** 0–1 fade for the hover ring. Omitted is fully visible. */
  hoverStrength?: number;
  /** 0–1 fade for the selection ring. Omitted is fully visible. */
  selectionStrength?: number;
  /** Commit HEAD resolves to, marked so "you are here" is visible in the lanes. */
  headCommitId?: string | null;
  /** CSS-pixel height of the canvas, used to cull off-screen edges. */
  viewportHeight?: number;
  /**
   * Draw only the hover/selection/HEAD rings, skipping lanes, connectors,
   * cutouts and node bodies. Used to overlay live emphasis on top of a
   * pre-rendered static layer (graphCache) without re-stroking the graph.
   */
  emphasisOnly?: boolean;
  /**
   * Draw author avatars in a dedicated column right of the lanes. The column
   * centre X is supplied by the caller (it owns gutter sizing) via
   * {@link RenderOptions.avatarX}; when either field is missing no avatars
   * are drawn. Avatars belong to the static layer: emphasis-only passes skip
   * them exactly like nodes.
   */
  showAvatars?: boolean;
  /** Canvas X of the avatar column centre; see {@link RenderOptions.showAvatars}. */
  avatarX?: number | null;
  /**
   * Skip connectors whose span exceeds {@link LOOKBACK_ROWS} (they are owned
   * by the live overlay; see graphEdges.ts). The strip painter must set this:
   * a long connector baked into a tile is clipped mid-edge at the seam, which
   * is exactly the floating-fragment artifact this split exists to prevent.
   * Default false — direct renders draw every edge themselves.
   */
  skipLongConnectors?: boolean;
}

/** Outer radius of the hover/selection ring at a given animation strength. */
export function emphasisRingRadius(nodeRadius: number, strength = 1): number {
  return nodeRadius + 2 + 1.5 * clamp01(strength);
}

/**
 * Author-avatar geometry per density. Radii are CSS pixels; the avatar disc
 * carries the author's initial so dense graphs stay attributable at a glance.
 */
export interface AvatarStyle {
  radius: number;
  fontPx: number;
}

export const AVATAR_STYLES: Record<DensityMode, AvatarStyle> = {
  spacious: { radius: 8, fontPx: 9 },
  compact: { radius: 6, fontPx: 7 },
};

/** Extra hit slop around the avatar disc, matching the node's forgiveness. */
export const AVATAR_HIT_SLOP = 3;

/** How far a dangling edge protrudes below its commit, as a fraction of a row. */
const DANGLING_STUB_RATIO = 0.62;
/**
 * Cubic-Bézier control offset approximating a circular quarter arc
 * (4/3·(√2−1)): connector corners are true tight quarter-turns, so the
 * straight rails on either side stay straight to the edge of the turn.
 */
const QUARTER_ARC_K = 0.5522847498;
/**
 * Rows of lookback for edges that start above the viewport and cross into it.
 * Doubles as the OWNERSHIP BOUNDARY of the static strip cache: connectors
 * with a span at or under this many rows are baked into tiles (the primed
 * range covers them, so no seam can clip mid-edge); anything longer is drawn
 * whole by the live overlay ({@link GraphRenderer.drawLongConnectors}),
 * because priming tiles back past this bound would blow the canvas-height
 * budget and leave the edge's middle drawn by nobody.
 */
export const LOOKBACK_ROWS = 60;

/**
 * The row window a strip painter must hand render() so connectors crossing
 * the strip's top seam stay continuous: LOOKBACK_ROWS above the strip's own
 * first row, down to its last. Single owner of that arithmetic — the cache
 * delivers firstRow/rowCount, callers go through here, so a change to either
 * side of the contract fails loudly instead of leaving hairline gaps at seams.
 */
export function primedRowRange(
  stripFirstRow: number,
  stripRowCount: number,
  totalRows: number,
): { from: number; to: number } {
  return {
    from: Math.max(0, stripFirstRow - LOOKBACK_ROWS),
    to: Math.min(totalRows, stripFirstRow + Math.max(0, stripRowCount - 1)),
  };
}

/**
 * A pass-through vertical: one colour, one x. Lanes are stable columns, so
 * a run can only ever be a straight line; it breaks when the lane's colour
 * changes (a recycled column's next occupant) or its rows stop being
 * contiguous.
 */
interface LaneRun {
  colorIndex: number;
  x: number;
  yTop: number;
  yBottom: number;
}

const RUN_POOL_MAX = 256;

/*
 * Scratch structures for render(). render() is synchronous and single-threaded,
 * so one module-scope set serves every call; they are reset on entry and their
 * run objects recycled through a pool instead of being reallocated per frame.
 */
const scratchOpenRuns = new Map<number, LaneRun>();
const scratchRuns: LaneRun[] = [];
const scratchSeenLanes: number[] = [];
const runPool: LaneRun[] = [];

function resetLaneScratch(): void {
  scratchOpenRuns.clear();
  scratchRuns.length = 0;
  scratchSeenLanes.length = 0;
}

function acquireRun(colorIndex: number, x: number, top: number, bottom: number): LaneRun {
  const recycled = runPool.pop();
  if (recycled) {
    recycled.colorIndex = colorIndex;
    recycled.x = x;
    recycled.yTop = top;
    recycled.yBottom = bottom;
    return recycled;
  }
  return { colorIndex, x, yTop: top, yBottom: bottom };
}

function releaseLaneScratch(): void {
  // The two collections MUST be disjoint before pooling: render()'s final
  // flush moves still-open runs into scratchRuns; if they stayed in the map
  // as well, each was pooled twice and the second acquireRun() of a recycled
  // object silently re-pointed a lane that another run still referenced —
  // losing lanes on every subsequent paint. Draining the map here makes the
  // invariant structural rather than call-site discipline.
  for (const run of scratchOpenRuns.values()) scratchRuns.push(run);
  resetLaneScratch();
  for (const run of scratchRuns) {
    if (runPool.length < RUN_POOL_MAX) runPool.push(run);
  }
  scratchRuns.length = 0;
}

/** What a graph pointer probe landed on. */
export type GraphHitKind = "node" | "lane" | "connector" | "avatar";

/**
 * A typed pointer hit on the graph gutter.
 *
 * `row` is always the commit the hit ATTRIBUTES to: the row's own commit
 * for nodes and avatars, the nearest occupant for a pass-through lane, and
 * — for connector hits — the CHILD commit that owns the descent (the
 * branch's last commit), with the merge point it lands on in
 * `connectorTarget`.
 */
export interface GraphHit {
  kind: GraphHitKind;
  row: VisualCommitRow;
  connectorTarget: VisualCommitRow | null;
}

/**
 * Row indices per lane (rows whose own `lane` is that lane), ascending.
 * Memoized on payload array identity — payloads are immutable snapshots,
 * so identity is a sound cache key and dropping the array drops the index.
 */
const laneOccupancyCache = new WeakMap<object, Map<number, number[]>>();

function laneOccupancy(rows: readonly VisualCommitRow[]): Map<number, number[]> {
  const hit = laneOccupancyCache.get(rows as object);
  if (hit) return hit;
  const map = new Map<number, number[]>();
  for (let i = 0; i < rows.length; i++) {
    const lane = rows[i].lane;
    if (!Number.isFinite(lane)) continue;
    let list = map.get(lane);
    if (!list) {
      list = [];
      map.set(lane, list);
    }
    list.push(i);
  }
  laneOccupancyCache.set(rows as object, map);
  return map;
}

/**
 * In-flight closing connectors per lane: for each closing edge (a live,
 * non-merge connection whose child leaves its own column to arrive on the
 * parent's), the rows its descent crosses EXCLUSIVE of both endpoints —
 * `[child+1, parent-1]` — during which the child's column shows nothing but
 * the drawn line.
 *
 * This index exists because those rows have no occupancy entry at all:
 * `active_lanes` lists nodes and pending reservations, and the solver
 * deliberately does not reserve visual occupancy for a descent (the column
 * is allocation-reserved, not drawn-through by a pass-through). Before it,
 * hovering the middle of a closing connector reported "nothing here" for a
 * line plainly on screen — the documented tooltip gap. Spans on one lane
 * are disjoint for solver output (column exclusivity), so lookup is a
 * binary search; hostile overlapping payloads degrade to a bounded
 * backward scan, never a wrong answer.
 *
 * Memoized on payload array identity like every other per-payload index.
 */
interface ClosingSpans {
  starts: number[];
  ends: number[];
  child: number[];
}

const closingSpanCache = new WeakMap<object, Map<number, ClosingSpans>>();

function closingSpansByLane(rows: readonly VisualCommitRow[]): Map<number, ClosingSpans> {
  const hit = closingSpanCache.get(rows as object);
  if (hit) return hit;
  const map = new Map<number, ClosingSpans>();
  for (let i = 0; i < rows.length; i++) {
    const conns = rows[i].connections;
    if (!conns) continue;
    for (const conn of conns) {
      if (conn.is_dangling || conn.is_merge) continue;
      if (!Number.isFinite(conn.from_lane) || conn.from_lane < 0) continue;
      if (conn.from_lane === conn.to_lane) continue;
      const target = connectionTargetIndex(i, conn.to_row_offset, rows.length);
      if (target === null || target <= i + 1) continue;
      let spans = map.get(conn.from_lane);
      if (!spans) {
        spans = { starts: [], ends: [], child: [] };
        map.set(conn.from_lane, spans);
      }
      // Build order ascends in child row, so starts are non-decreasing and
      // binary search needs no sort.
      spans.starts.push(i + 1);
      spans.ends.push(target - 1);
      spans.child.push(i);
    }
  }
  closingSpanCache.set(rows as object, map);
  return map;
}

/** The closing connector crossing `rowIdx` on `lane`, or null. */
function findClosingConnector(
  spansByLane: Map<number, ClosingSpans>,
  rowIdx: number,
  lane: number,
): { child: number; target: number } | null {
  const spans = spansByLane.get(lane);
  if (!spans || spans.starts.length === 0) return null;
  // Greatest start <= rowIdx.
  let lo = 0;
  let hi = spans.starts.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (spans.starts[mid] <= rowIdx) lo = mid + 1;
    else hi = mid;
  }
  // Solver spans on one lane are disjoint, so only the last-starting
  // candidate can cover rowIdx; the backward scan exists for hostile
  // overlapping payloads and is bounded by the lane's own list.
  for (let k = lo - 1; k >= 0; k--) {
    if (spans.ends[k] >= rowIdx) {
      return { child: spans.child[k], target: spans.ends[k] + 1 };
    }
  }
  return null;
}

/**
 * Nearest commit whose `lane` is `logical` (ties prefer the row below —
 * a pass-through's line leads down to its pending commit).
 *
 * Resolved through the memoized occupancy index in O(log n): a stable
 * column's reservation can legitimately span thousands of rows between two
 * commits of one branch, and the old distance-capped walk reported "no
 * occupant" past its cap — indistinguishable from an honest miss, so long
 * pass-throughs silently stopped being hoverable.
 */
function findLaneOccupant(
  rows: readonly VisualCommitRow[],
  fromIdx: number,
  logical: number,
): VisualCommitRow | null {
  const list = laneOccupancy(rows).get(logical);
  if (!list || list.length === 0) return null;
  let lo = 0;
  let hi = list.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (list[mid] < fromIdx) lo = mid + 1;
    else hi = mid;
  }
  const below = lo < list.length ? list[lo] : -1;
  const above = lo > 0 ? list[lo - 1] : -1;
  if (below < 0) return above < 0 ? null : rows[above];
  if (above < 0) return rows[below];
  return below - fromIdx <= fromIdx - above ? rows[below] : rows[above];
}

export class GraphRenderer {
  private config: GraphRenderConfig;
  /** Avatar disc metrics; follows the active density so compact rows shrink. */
  private avatarStyle: AvatarStyle = AVATAR_STYLES.spacious;

  constructor(config: Partial<GraphRenderConfig> = {}) {
    this.config = { ...DEFAULT_CONFIG, ...config };
  }

  public getConfig(): GraphRenderConfig {
    return { ...this.config };
  }

  public setConfig(config: Partial<GraphRenderConfig>): void {
    this.config = { ...this.config, ...config };
  }

  public setDensity(mode: DensityMode): void {
    this.config = { ...this.config, ...DENSITY_CONFIGS[mode] };
    this.avatarStyle = AVATAR_STYLES[mode];
  }

  /** The avatar metrics matching the current density (exposed for hit-testing parity). */
  public getAvatarStyle(): AvatarStyle {
    return { ...this.avatarStyle };
  }


  /**
   * Computes the Y-coordinate on the canvas for a given row index.
   *
   * Single owner of row arithmetic: render(), dangling stubs, avatars and
   * hit-testing all resolve coordinates through here. The old dual-mode
   * (absolute/relative) branch was unreachable for every real caller — both
   * modes coincide or absolute always won — and keeping it invited drift
   * between paint and hit-testing.
   */
  public getRowY(rowIdx: number, scrollOffset: number = 0): number {
    const { rowHeight } = this.config;
    return rowIdx * rowHeight + rowHeight / 2 - scrollOffset;
  }

  /**
   * Canvas X of a solver lane. Lanes are stable columns for a branch's
   * whole lifetime, so this mapping holds on every row — there is no
   * per-row repacking that could make a lane's x depend on its neighbours.
   */
  public getLaneX(lane: number): number {
    const { originX, laneWidth } = this.config;
    return originX + lane * laneWidth;
  }

  /**
   * Canvas X of a lane on a given row. Identical to {@link getLaneX} —
   * kept so callers written against the row-dependent era keep working,
   * and as the single place to say why the row no longer matters.
   */
  public laneXForRow(_row: VisualCommitRow, logicalLane: number): number {
    return this.getLaneX(logicalLane);
  }

  /**
   * Row-index → canvas-y closure shared by every painter and by
   * getCommitAtPoint, so the cached static layer, live overlay and hit test
   * can never disagree about where a row sits.
   */
  private yForRow(scrollOffset: number): (r: number) => number {
    return (r: number) => this.getRowY(r, scrollOffset);
  }

  /**
   * The commit whose node, branch lane, connector, or author avatar covers
   * a canvas point, or null. Compatibility wrapper over
   * {@link getGraphHitAtPoint}: callers that only need the attributed
   * commit keep their one-value contract.
   */
  public getCommitAtPoint(
    x: number,
    y: number,
    rows: VisualCommitRow[],
    startIndex: number,
    endIndex: number,
    scrollOffset: number = 0,
    avatarX: number | null = null,
  ): VisualCommitRow | null {
    return (
      this.getGraphHitAtPoint(x, y, rows, startIndex, endIndex, scrollOffset, avatarX)?.row ??
      null
    );
  }

  /**
   * Typed pointer hit on the graph gutter, or null.
   *
   * Nodes are tiny; the coloured lane through the row is what people
   * actually hover. In priority order a probe resolves to: the row's own
   * node column; a pass-through lane's nearest occupant; an in-flight
   * closing connector (attributed to the CHILD commit whose branch the
   * descent belongs to, with the merge point in `connectorTarget`); the
   * author avatar column when `avatarX` is supplied. Occupancy always
   * outranks a connector claim — on solver output the two can never
   * coincide (column exclusivity), so the ordering only disciplines
   * hostile payloads.
   */
  public getGraphHitAtPoint(
    x: number,
    y: number,
    rows: VisualCommitRow[],
    startIndex: number,
    endIndex: number,
    scrollOffset: number = 0,
    avatarX: number | null = null,
  ): GraphHit | null {
    const { nodeRadius, mergeNodeRadius, rowHeight, lineWidth, originX, laneWidth } =
      this.config;
    if (!rows || rows.length === 0 || rowHeight <= 0) return null;
    if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
    const avatarCenterX =
      avatarX !== null && Number.isFinite(avatarX) ? avatarX : null;
    const avatar = avatarCenterX === null ? null : this.avatarStyle;
    const lo = Math.max(0, startIndex);
    const limit = Math.min(rows.length, Math.max(endIndex, startIndex));
    const idx = Math.floor((y + scrollOffset) / rowHeight);
    if (!Number.isFinite(idx) || idx < lo || idx >= limit) return null;

    const row = rows[idx];
    const live = liveLanesByRow(rows)[idx] ?? collectLiveLanes(row);
    const slop = Math.max(
      (row.is_merge ? mergeNodeRadius : nodeRadius) + 4,
      lineWidth / 2 + 4,
    );
    const nodeX = this.getLaneX(row.lane);
    if (Math.abs(x - nodeX) <= slop) {
      return { kind: "node", row, connectorTarget: null };
    }

    for (const logical of live) {
      if (logical === row.lane) continue;
      const laneX = this.getLaneX(logical);
      if (Math.abs(x - laneX) > slop) continue;
      const occupant = findLaneOccupant(rows, idx, logical);
      if (occupant) return { kind: "lane", row: occupant, connectorTarget: null };
    }

    // Closing connectors in flight: the pointer lane is derived from x
    // (those columns have no occupancy entry to iterate).
    if (laneWidth > 0) {
      const probeLane = Math.round((x - originX) / laneWidth);
      if (
        Number.isFinite(probeLane) &&
        probeLane >= 0 &&
        Math.abs(x - this.getLaneX(probeLane)) <= slop
      ) {
        const inFlight = findClosingConnector(closingSpansByLane(rows), idx, probeLane);
        if (inFlight && inFlight.child >= 0 && inFlight.child < rows.length) {
          return {
            kind: "connector",
            row: rows[inFlight.child],
            connectorTarget:
              inFlight.target >= 0 && inFlight.target < rows.length
                ? rows[inFlight.target]
                : null,
          };
        }
      }
    }

    if (avatarCenterX !== null && avatar) {
      const ar = avatar.radius + AVATAR_HIT_SLOP;
      if (Math.abs(x - avatarCenterX) <= ar) {
        return { kind: "avatar", row, connectorTarget: null };
      }
    }
    return null;
  }

  /**
   * The canvas width this graph needs, so the gutter can be sized to the
   * history instead of clipping deep branches at a fixed width. Width is the
   * highest occupied column ({@link maxOccupiedLane}); the solver's interval
   * allocation keeps that equal to peak concurrent occupancy, so transient
   * holes never cost extra width beyond what the history genuinely needs.
   */
  public measureWidth(rows: VisualCommitRow[]): number {
    const { originX, laneWidth, nodeRadius } = this.config;
    const maxLane = maxOccupiedLane(rows);
    return originX + maxLane * laneWidth + nodeRadius + originX;
  }


  public render(
    ctx: CanvasRenderingContext2D,
    rows: VisualCommitRow[],
    startIndex: number,
    endIndex: number,
    scrollOffset: number = 0,
    selectedCommitId?: string,
    optionsOrHeadId?: RenderOptions | string | null,
    hoveredCommitId?: string | null
  ) {
    const { rowHeight, nodeRadius, mergeNodeRadius, lineWidth } = this.config;
    let options: RenderOptions = {};
    if (typeof optionsOrHeadId === "string" || optionsOrHeadId === null) {
      options = {
        headCommitId: optionsOrHeadId,
        hoveredCommitId: hoveredCommitId ?? null,
      };
    } else if (optionsOrHeadId) {
      options = optionsOrHeadId;
    }
    const theme = options.theme ?? DEFAULT_THEME;

    resetLaneScratch();

    if (!rows || rows.length === 0) return;

    ctx.save();
    try {
      ctx.lineWidth = lineWidth;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.imageSmoothingEnabled = true;

    // Row arithmetic resolves through the shared yForRow closure; every
    // painter and the hit test see identical coordinates by construction.
    const calcY = this.yForRow(scrollOffset);

    const viewportHeight = options.viewportHeight ?? this.canvasHeight(ctx);
    // The connector loop runs five rows past endIndex so partially-scrolled
    // bottom rows render; the exact-extension query below MUST use that same
    // bound, or an edge landing inside the fudge band loses its upper segment
    // whenever its child sits beyond the fixed lookback.
    //
    // When this pass owns long connectors (any direct render), the window
    // also extends EXACTLY to the deepest child that targets a rendered row,
    // so an edge of any span draws whole from its own child row — there is
    // no scan cap left to silently drop history behind.
    //
    // Strip painters pass skipLongConnectors and hand us their PRIMED range:
    // primedRowRange owns the seam arithmetic, so this pass trusts the given
    // bounds verbatim. Extending further here would stack a second lookback
    // on top of the painter's and turn a priming regression into silent
    // over-draw instead of a caught failure.
    const renderEnd = Math.min(rows.length, endIndex + 5);
    let renderStart = startIndex;
    if (!options.skipLongConnectors) {
      // Deepest child whose edge LANDS in the window…
      let deepest = deepestChildTargetingRange(
        buildIncomingEdgeIndex(rows),
        startIndex,
        renderEnd,
      );
      // …and children of long edges whose spans CROSS the window without
      // landing in it: their strokes are this pass's responsibility too,
      // and only reaching back to the child row draws them whole.
      for (const e of collectLongEdges(rows, LOOKBACK_ROWS)) {
        if (e.child < startIndex && e.target >= renderEnd && e.child < deepest) {
          deepest = e.child;
        }
      }
      renderStart = Math.min(Math.max(0, startIndex - LOOKBACK_ROWS), deepest);
    }

    // 1. Pass-through lanes, drawn as one straight vertical per unbroken run.
    //
    // Stroking a separate segment per row leaves a visible seam at every row
    // boundary once the line has any transparency or antialiasing, and costs
    // a path per row per lane. Lanes are stable columns, so a run only ever
    // extends downward at one x; it breaks when the lane's colour changes (a
    // recycled column's next occupant) or its rows stop being contiguous.
    if (!options.emphasisOnly) {
      for (let i = startIndex; i < renderEnd && i < rows.length; i++) {
        const row = rows[i];
        const yCenter = calcY(i);
        const yTop = yCenter - rowHeight / 2;
        const yBottom = yCenter + rowHeight / 2;
        scratchSeenLanes.length = 0;

        for (let l = 0; l < row.active_lanes.length; l++) {
          const lane = row.active_lanes[l];
          if (lane === row.lane) continue;
          if (!scratchSeenLanes.includes(lane)) scratchSeenLanes.push(lane);
          const colorIndex = row.active_lane_colors[l] ?? lane;
          const open = scratchOpenRuns.get(lane);
          if (
            open &&
            open.colorIndex === colorIndex &&
            Math.abs(open.yBottom - yTop) < 0.5
          ) {
            open.yBottom = yBottom;
          } else {
            if (open) scratchRuns.push(open);
            scratchOpenRuns.set(lane, acquireRun(colorIndex, this.getLaneX(lane), yTop, yBottom));
          }
        }
        for (const [lane, run] of scratchOpenRuns) {
          if (!scratchSeenLanes.includes(lane)) {
            scratchRuns.push(run);
            scratchOpenRuns.delete(lane);
          }
        }
      }
      // Flush surviving runs and drain the map in the same pass: leaving
      // entries here made releaseLaneScratch pool each of them twice, which
      // aliased two lanes onto one recycled object on the next paint.
      for (const [lane, run] of scratchOpenRuns) {
        scratchRuns.push(run);
        scratchOpenRuns.delete(lane);
      }

      for (const run of scratchRuns) {
        ctx.strokeStyle = getBranchColor(run.colorIndex);
        ctx.beginPath();
        ctx.moveTo(run.x, run.yTop);
        ctx.lineTo(run.x, run.yBottom);
        ctx.stroke();
      }
    }

    // 2. Connectors: a vertical run with one rounded corner, not an S over the
    //    whole span. The corner sits at the end that owns the lane change — a
    //    merged-in branch peels away just under its merge commit, and a lane
    //    that closes stays straight until it arrives at its parent — which is
    //    what makes a dense graph readable rather than a bundle of diagonals.
    if (!options.emphasisOnly)
      for (let i = renderStart; i < renderEnd && i < rows.length; i++) {
        const row = rows[i];
        const yFrom = calcY(i);

        for (const conn of row.connections) {
          if (conn.is_dangling) {
            // Dangling parents draw as fade stubs in the live overlay
            // (drawDanglingStubs), never here: baked into strip tiles the
            // translucent geometry would clip at every strip edge and show
            // alpha seams. See paintGraphFrame for the frame composition.
            continue;
          }
          if (options.skipLongConnectors && isLongConnection(conn, LOOKBACK_ROWS)) {
            // Owned by drawLongConnectors: baking a span longer than the
            // primed lookback into a tile guarantees a mid-edge seam clip.
            continue;
          }

          const targetRowIdx = connectionTargetIndex(i, conn.to_row_offset, rows.length);
          if (targetRowIdx === null) continue;

          const yTo = calcY(targetRowIdx);
          const fromLane = Number.isFinite(conn.from_lane) ? conn.from_lane : row.lane;
          if (!Number.isFinite(fromLane) || !Number.isFinite(conn.to_lane)) continue;

          if (Math.max(yFrom, yTo) < -rowHeight || Math.min(yFrom, yTo) > viewportHeight + rowHeight) {
            continue;
          }

          ctx.strokeStyle = getBranchColor(conn.color_index);
          ctx.beginPath();
          this.strokeConnector(
            ctx,
            this.getLaneX(fromLane),
            yFrom,
            this.getLaneX(conn.to_lane),
            yTo,
            conn.is_merge,
          );
          ctx.stroke();
        }
      }

    // 3. Nodes.
    for (let i = startIndex; i < renderEnd && i < rows.length; i++) {
      const row = rows[i];
      const y = calcY(i);
      if (y < -rowHeight || y > viewportHeight + rowHeight) continue;
      const isSelected = selectedCommitId === row.id;
      const isHead = options.headCommitId === row.id;
      const isHovered = options.hoveredCommitId === row.id;
      if (options.emphasisOnly && !isSelected && !isHead && !isHovered) continue;
      const x = this.getLaneX(row.lane);
      const color = getBranchColor(row.color_index);
      const r = row.is_merge ? mergeNodeRadius : nodeRadius;
      const hoverStrength = clamp01(options.hoverStrength ?? 1);
      const selectionStrength = clamp01(options.selectionStrength ?? 1);

      // Cutout so lanes passing behind the node do not touch it. Skipped in
      // emphasis-only mode: punching a hole here would erase the static layer.
      if (!options.emphasisOnly) this.paintNodeCutout(ctx, x, y, r, theme);

      if (isHovered && !isSelected && hoverStrength > 0) {
        const previousAlpha = ctx.globalAlpha ?? 1;
        ctx.globalAlpha = previousAlpha * hoverStrength;
        ctx.strokeStyle = theme.muted;
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.arc(x, y, emphasisRingRadius(r, hoverStrength), 0, Math.PI * 2);
        ctx.stroke();
        ctx.globalAlpha = previousAlpha;
      }

      if (isSelected && selectionStrength > 0) {
        ctx.strokeStyle = theme.selection;
        ctx.lineWidth = 1 + selectionStrength;
        ctx.beginPath();
        ctx.arc(x, y, emphasisRingRadius(r, selectionStrength), 0, Math.PI * 2);
        ctx.stroke();
      }

      // HEAD is marked in the lanes as well as in the ref chips: the chip says
      // which branch, the ring says which row your working tree is on.
      if (isHead) {
        ctx.strokeStyle = theme.head;
        ctx.lineWidth = 1.5;
        ctx.beginPath();
        ctx.arc(x, y, r + (isSelected ? 6 : 5.5), 0, Math.PI * 2);
        ctx.stroke();
      }

      if (options.emphasisOnly) continue;

      this.paintNodeBodyAndMarks(ctx, row, x, y, color, theme);
    }
    // 4. Author avatars. A dedicated column right of the lanes: the caller
    //    owns its X (it sizes the gutter), rows align by node Y, so authorship
    //    stays attributable without ever colliding with dense lane traffic.
    //    Static-layer only — emphasis passes must not stamp avatars over
    //    blitted strips.
    if (!options.emphasisOnly && options.showAvatars && options.avatarX !== null && options.avatarX !== undefined) {
      const avatarX = options.avatarX;
      if (Number.isFinite(avatarX)) {
        for (let i = startIndex; i < renderEnd && i < rows.length; i++) {
          const row = rows[i];
          const y = calcY(i);
          if (y < -rowHeight || y > viewportHeight + rowHeight) continue;
          this.drawAuthorAvatar(ctx, avatarX, y, this.avatarStyle.radius, this.avatarStyle.fontPx, theme, row);
        }
      }
    }
  } finally {
    ctx.restore();
    releaseLaneScratch();
  }
}

  /**
   * Draws the fading stubs for commits whose parent lies outside the loaded
   * window — the one owner of stub geometry, so the cached static layer and
   * this overlay cannot drift (render() itself skips dangling connections).
   *
   * Stubs are translucent on purpose; rasterized into per-strip tiles they
   * would clip mid-fade at every strip boundary and leave alpha seams where
   * tiles meet. Drawn here they land whole, uncropped, on the single visible
   * surface, between the strip blits and the emphasis rings.
   */
  public drawDanglingStubs(
    ctx: CanvasRenderingContext2D,
    rows: VisualCommitRow[],
    startIndex: number,
    endIndex: number,
    scrollOffset: number = 0,
    viewportHeight?: number,
  ): void {
    if (!rows || rows.length === 0) return;
    const { rowHeight } = this.config;
    const viewHeight = viewportHeight ?? this.canvasHeight(ctx);
    const calcY = this.yForRow(scrollOffset);

    ctx.save();
    ctx.lineWidth = this.config.lineWidth;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    try {
      for (let i = startIndex; i < endIndex && i < rows.length; i++) {
        const row = rows[i];
        if (!row.connections.some((conn) => conn.is_dangling)) continue;
        const yFrom = calcY(i);
        // Same cull band as every other row-anchored mark.
        if (yFrom < -rowHeight || yFrom > viewHeight + rowHeight) continue;
        for (const conn of row.connections) {
          if (!conn.is_dangling) continue;
          const fromLane = Number.isFinite(conn.from_lane) ? conn.from_lane : row.lane;
          // A live straight-down edge on the same lane (a promoted
          // continuation after a window-cut first parent) fully covers the
          // stub: translucent geometry over a solid line conveys nothing
          // and muddies the lane. Covered stubs are skipped; a stub beside
          // a departing close still draws — it marks the missing parent.
          const covered = row.connections.some(
            (other) =>
              !other.is_dangling &&
              other.from_lane === fromLane &&
              other.to_lane === fromLane,
          );
          if (covered) continue;
          this.strokeDanglingStub(
            ctx,
            this.laneXForRow(row, fromLane),
            yFrom,
            rowHeight,
            conn.color_index,
          );
        }
      }
    } finally {
      ctx.restore();
    }
  }

  /**
   * One stub: a two-segment fade out of the commit's underside — solid near
   * the commit, a whisper at the tip — restored to the caller's alpha so no
   * later mark inherits the fade.
   */
  private strokeDanglingStub(
    ctx: CanvasRenderingContext2D,
    xFrom: number,
    yFrom: number,
    rowHeight: number,
    colorIndex: number,
  ): void {
    const previousAlpha = ctx.globalAlpha ?? 1;
    ctx.strokeStyle = getBranchColor(colorIndex);
    ctx.globalAlpha = previousAlpha * 0.55;
    ctx.beginPath();
    ctx.moveTo(xFrom, yFrom);
    ctx.lineTo(xFrom, yFrom + rowHeight * DANGLING_STUB_RATIO * 0.6);
    ctx.stroke();
    ctx.globalAlpha = previousAlpha * 0.22;
    ctx.beginPath();
    ctx.moveTo(xFrom, yFrom + rowHeight * DANGLING_STUB_RATIO * 0.6);
    ctx.lineTo(xFrom, yFrom + rowHeight * DANGLING_STUB_RATIO);
    ctx.stroke();
    ctx.globalAlpha = previousAlpha;
  }

  /**
   * One author avatar: a hue-coded disc carrying the author's initials, ringed
   * so it separates from the background in either theme. Identity resolution
   * (hash, hue, initials) is owned by authors/authorIdentity — the renderer
   * only places pixels, so canvas avatars can never drift from their DOM
   * counterparts.
   */
  private drawAuthorAvatar(
    ctx: CanvasRenderingContext2D,
    x: number,
    y: number,
    radius: number,
    fontPx: number,
    theme: GraphTheme,
    row: VisualCommitRow,
  ): void {
    const identity = authorIdentity(row.author_name, row.author_email);
    ctx.save();
    try {
      // Ring first: keeps the disc legible over same-hue lane lines beneath.
      ctx.strokeStyle = theme.nodeStroke;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.arc(x, y, radius + 1.25, 0, Math.PI * 2);
      ctx.stroke();

      ctx.fillStyle = authorColor(identity.hue);
      ctx.beginPath();
      ctx.arc(x, y, radius, 0, Math.PI * 2);
      ctx.fill();

      if (radius >= 5 && identity.initials) {
        ctx.fillStyle = "#ffffff";
        ctx.font = `700 ${fontPx}px ui-sans-serif, system-ui, -apple-system, sans-serif`;
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillText(identity.initials, x, y + fontPx * 0.05);
      }
    } finally {
      ctx.restore();
    }
  }

  /** The background disc punched under a node so lanes do not touch it. */
  private paintNodeCutout(
    ctx: CanvasRenderingContext2D,
    x: number,
    y: number,
    r: number,
    theme: GraphTheme,
  ): void {
    ctx.fillStyle = theme.background;
    ctx.beginPath();
    ctx.arc(x, y, r + 2.5, 0, Math.PI * 2);
    ctx.fill();
  }

  /**
   * Node body and shape marks (hollow merge / ringed root), shared by the
   * render loop and the long-connector repair pass so both produce
   * pixel-identical nodes. Restores the configured line width on exit.
   */
  private paintNodeBodyAndMarks(
    ctx: CanvasRenderingContext2D,
    row: VisualCommitRow,
    x: number,
    y: number,
    color: string,
    theme: GraphTheme,
  ): void {
    const { nodeRadius, mergeNodeRadius, lineWidth } = this.config;
    const r = row.is_merge ? mergeNodeRadius : nodeRadius;
    ctx.fillStyle = color;
    ctx.strokeStyle = theme.nodeStroke;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.arc(x, y, r, 0, Math.PI * 2);
    ctx.fill();
    ctx.stroke();

    // A merge is hollow and a root is ringed: two shapes a reader can tell
    // apart at a glance, in either theme, without a legend.
    if (row.is_merge) {
      ctx.fillStyle = theme.background;
      ctx.beginPath();
      ctx.arc(x, y, Math.max(1.4, r * 0.42), 0, Math.PI * 2);
      ctx.fill();
    } else if (row.is_root) {
      ctx.strokeStyle = color;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.arc(x, y, r + 2, 0, Math.PI * 2);
      ctx.stroke();
    }

    ctx.lineWidth = lineWidth;
  }

  /**
   * Draws the connectors the static strip cache does not own — every edge
   * whose span exceeds {@link LOOKBACK_ROWS} and lands on a row in
   * `[startIndex, endIndex)` — whole, regardless of how far above the window
   * its child sits.
   *
   * Why these cannot live in the tiles: the tile showing an edge's parent
   * would have to rasterize all the way back to the child, and a span longer
   * than the primed lookback leaves the edge's middle drawn by NO tile — the
   * floating line fragments at strip seams this pass exists to kill. The
   * incoming-edge index makes enumeration exact and bounded by what is
   * visible: O(edges touching the window) per frame, never O(history).
   *
   * Edges are stroked OVER the blitted strips, so afterwards every visible
   * node the drawn geometry can touch gets its cutout and body re-stamped —
   * the same under-lanes/over-nothing relationship nodes had inside the
   * strips, restored per frame for just the affected rows.
   */
  public drawLongConnectors(
    ctx: CanvasRenderingContext2D,
    rows: VisualCommitRow[],
    startIndex: number,
    endIndex: number,
    scrollOffset: number = 0,
    viewportHeight?: number,
    options?: RenderOptions,
  ): void {
    if (!rows || rows.length === 0) return;
    const { rowHeight, lineWidth } = this.config;
    const theme = options?.theme ?? DEFAULT_THEME;
    const viewHeight = viewportHeight ?? this.canvasHeight(ctx);
    const calcY = this.yForRow(scrollOffset);

    const lo = Math.max(0, startIndex);
    const hi = Math.min(rows.length, Math.max(endIndex, startIndex));

    // Every long edge whose span INTERSECTS the window — landing in it,
    // starting in it, or crossing clean through. Enumerating only edges
    // that LAND here (the old incoming-index walk) dropped the other two
    // cases; a closing descent has no occupancy entries, so its lane
    // simply vanished for any window strictly inside the span.
    type VisibleLongEdge = {
      j: number;
      t: number;
      fromLane: number;
      toLane: number;
      yFrom: number;
      yTo: number;
      colorIndex: number;
      isMerge: boolean;
    };
    const edges: VisibleLongEdge[] = [];
    for (const e of collectLongEdges(rows, LOOKBACK_ROWS)) {
      if (e.child >= hi || e.target < lo) continue;
      const yFrom = calcY(e.child);
      const yTo = calcY(e.target);
      if (
        Math.max(yFrom, yTo) < -rowHeight ||
        Math.min(yFrom, yTo) > viewHeight + rowHeight
      ) {
        continue;
      }
      edges.push({
        j: e.child,
        t: e.target,
        fromLane: e.fromLane,
        toLane: e.toLane,
        yFrom,
        yTo,
        colorIndex: e.colorIndex,
        isMerge: e.isMerge,
      });
    }
    if (edges.length === 0) return;

    ctx.save();
    try {
      ctx.lineWidth = lineWidth;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";

      for (const e of edges) {
        ctx.strokeStyle = getBranchColor(e.colorIndex);
        ctx.beginPath();
        this.strokeConnector(
          ctx,
          this.getLaneX(e.fromLane),
          e.yFrom,
          this.getLaneX(e.toLane),
          e.yTo,
          e.isMerge,
        );
        ctx.stroke();
      }

      // Re-stamp every visible node the drawn geometry could overlap:
      // endpoints (the stroke starts and ends on their centres) and any node
      // whose disc sits inside an edge's bounding box.
      const { nodeRadius, mergeNodeRadius } = this.config;
      const pad = Math.max(nodeRadius, mergeNodeRadius) + 2.5 + lineWidth;
      const affected = new Set<number>();
      for (const e of edges) {
        const yMin = Math.min(e.yFrom, e.yTo) - pad;
        const yMax = Math.max(e.yFrom, e.yTo) + pad;
        // The edge's corner sweeps horizontally near one endpoint, so the
        // restamp band conservatively covers every visible row the edge
        // crosses, whatever column their nodes sit on.
        for (let t = lo; t < hi; t++) {
          if (t < e.j || t > e.t) continue;
          const y = calcY(t);
          if (y < yMin || y > yMax) continue;
          affected.add(t);
        }
      }
      const ordered = [...affected].sort((a, b) => a - b);
      for (const t of ordered) {
        const row = rows[t];
        const x = this.getLaneX(row.lane);
        const y = calcY(t);
        if (y < -rowHeight || y > viewHeight + rowHeight) continue;
        const r = row.is_merge ? mergeNodeRadius : nodeRadius;
        this.paintNodeCutout(ctx, x, y, r, theme);
        this.paintNodeBodyAndMarks(ctx, row, x, y, getBranchColor(row.color_index), theme);
      }
    } finally {
      ctx.restore();
    }
  }

  /**
   * Child→parent stroke between two stable columns: a straight vertical
   * when the columns agree, otherwise a RAIL — straight horizontal run,
   * one tight quarter-turn, straight vertical run. The horizontal run sits
   * on the row that owns the lane change: a merged-in branch peels along
   * its merge commit's row and then descends its own column; a closing
   * lane descends straight and approaches along its parent's row. Because
   * the horizontal legs live exactly on row centrelines, every edge landing
   * on one commit shares a single approach line — dense confluences read
   * as rails, never as a braid of crossing diagonals. Column exclusivity
   * is the solver's contract: nothing else ever occupies the span this
   * stroke descends through, so there is no per-row occupancy to track.
   *
   * A non-forward edge (hostile offset placing the parent at or above the
   * child) degrades to a plain line so corrupt input cannot bend geometry
   * into neighbouring rows.
   */
  private strokeConnector(
    ctx: CanvasRenderingContext2D,
    xFrom: number,
    yFrom: number,
    xTo: number,
    yTo: number,
    isMerge: boolean,
  ): void {
    if (Math.abs(xFrom - xTo) < 0.5 || yTo <= yFrom) {
      ctx.moveTo(xFrom, yFrom);
      ctx.lineTo(xTo, yTo);
      return;
    }
    if (isMerge) {
      this.cornerAtStart(ctx, xFrom, yFrom, xTo, yTo);
    } else {
      this.cornerAtEnd(ctx, xFrom, yFrom, xTo, yTo);
    }
  }

  /**
   * Corner on the child's row: the peel runs HORIZONTALLY along the child's
   * own row to the target column, takes one tight quarter-turn, then runs
   * straight down. The old shape — a cubic from endpoint to endpoint —
   * smeared the entire horizontal traversal diagonally across the span;
   * with several wide peels per screen that read as spaghetti, not rails.
   */
  private cornerAtStart(
    ctx: CanvasRenderingContext2D,
    xFrom: number,
    yFrom: number,
    xTo: number,
    yTo: number
  ) {
    const radius = this.cornerRadius(yTo - yFrom, xTo - xFrom);
    if (radius < 0.75) {
      // Sub-pixel corner: the whole edge fits in a band thinner than the
      // turn could render. A plain line is identical on screen.
      ctx.moveTo(xFrom, yFrom);
      ctx.lineTo(xTo, yTo);
      return;
    }
    const sgn = xTo > xFrom ? 1 : -1;
    ctx.moveTo(xFrom, yFrom);
    const turnStartX = xTo - sgn * radius;
    if (Math.abs(turnStartX - xFrom) > 0.1) ctx.lineTo(turnStartX, yFrom);
    ctx.bezierCurveTo(
      xTo - sgn * radius * (1 - QUARTER_ARC_K),
      yFrom,
      xTo,
      yFrom + radius * (1 - QUARTER_ARC_K),
      xTo,
      yFrom + radius,
    );
    if (yTo > yFrom + radius + 0.1) ctx.lineTo(xTo, yTo);
  }

  /**
   * Corner on the parent's row: the close runs straight down its own column,
   * takes one tight quarter-turn, then approaches HORIZONTALLY along the
   * parent's row. Every edge landing on one row therefore shares the same
   * approach line and simultaneous landings coincide instead of braiding —
   * the scholarlm confluence artifact.
   */
  private cornerAtEnd(
    ctx: CanvasRenderingContext2D,
    xFrom: number,
    yFrom: number,
    xTo: number,
    yTo: number
  ) {
    const radius = this.cornerRadius(yTo - yFrom, xTo - xFrom);
    if (radius < 0.75) {
      ctx.moveTo(xFrom, yFrom);
      ctx.lineTo(xTo, yTo);
      return;
    }
    const sgn = xTo > xFrom ? 1 : -1;
    ctx.moveTo(xFrom, yFrom);
    const turnStartY = yTo - radius;
    if (turnStartY > yFrom + 0.1) ctx.lineTo(xFrom, turnStartY);
    ctx.bezierCurveTo(
      xFrom,
      yTo - radius * (1 - QUARTER_ARC_K),
      xFrom + sgn * radius * (1 - QUARTER_ARC_K),
      yTo,
      xFrom + sgn * radius,
      yTo,
    );
    if (Math.abs(xTo - (xFrom + sgn * radius)) > 0.1) ctx.lineTo(xTo, yTo);
  }

  /**
   * The quarter-turn's radius: at most half a row (so the straight vertical
   * dominates and hovering the descent matches the closing-span hit model),
   * half the horizontal gap and half the vertical span (so neither the
   * horizontal run nor the vertical run can be consumed by the turn, and the
   * corner can never overshoot the parent).
   */
  private cornerRadius(deltaY: number, deltaX: number): number {
    const { rowHeight } = this.config;
    return Math.min(rowHeight * 0.5, Math.abs(deltaX) / 2, Math.abs(deltaY) / 2);
  }

  private canvasHeight(ctx: CanvasRenderingContext2D): number {
    const height = ctx.canvas?.height;
    if (!height) return 3000;
    const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
    return height / dpr;
  }
}
