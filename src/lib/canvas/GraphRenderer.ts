import { clamp01 } from "../motion/easing";
import { getBranchColor } from "./Palette";
import { authorColor, authorIdentity } from "../authors/authorIdentity";
import {
  buildIncomingEdgeIndex,
  deepestChildTargetingRange,
  isLongConnection,
} from "./graphEdges";

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

/** A vertical pass-through segment: one lane, one colour, one y-span. */
interface LaneRun {
  lane: number;
  colorIndex: number;
  top: number;
  bottom: number;
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

function acquireRun(lane: number, colorIndex: number, top: number, bottom: number): LaneRun {
  const recycled = runPool.pop();
  if (recycled) {
    recycled.lane = lane;
    recycled.colorIndex = colorIndex;
    recycled.top = top;
    recycled.bottom = bottom;
    return recycled;
  }
  return { lane, colorIndex, top, bottom };
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
   * The canvas X of a lane's centre line.
   */
  public getLaneX(lane: number): number {
    const { originX, laneWidth } = this.config;
    return originX + lane * laneWidth;
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
   * The commit whose node — or author avatar — covers a canvas point, or null.
   *
   * The hit area is the node plus a few pixels: a 5px disc is a hard target
   * with a mouse and an impossible one on a trackpad, and a click that lands
   * one pixel out should still select the commit the user aimed at. When
   * `avatarX` is supplied the avatar column is hit-tested with the same
   * slop, so hovering an avatar yields the same tooltip as its node.
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
    const { nodeRadius, mergeNodeRadius } = this.config;
    if (!rows || rows.length === 0) return null;
    // Normalize once: the loop below reads the center X on every hit check,
    // and a bare truthiness guard on `avatar` does not narrow `avatarX`.
    const avatarCenterX =
      avatarX !== null && Number.isFinite(avatarX) ? avatarX : null;
    const avatar = avatarCenterX === null ? null : this.avatarStyle;
    const limit = Math.min(rows.length, Math.max(endIndex, startIndex));

    for (let i = startIndex; i < limit; i++) {
      const row = rows[i];
      const nodeY = this.getRowY(i, scrollOffset);
      // Node first: it is the primary target when both discs overlap a point.
      const nodeX = this.getLaneX(row.lane);
      const radius = (row.is_merge ? mergeNodeRadius : nodeRadius) + 4;
      const dx = x - nodeX;
      const dy = y - nodeY;
      if (dx * dx + dy * dy <= radius * radius) {
        return row;
      }
      if (avatarCenterX !== null && avatar) {
        const adx = x - avatarCenterX;
        const ar = avatar.radius + AVATAR_HIT_SLOP;
        if (adx * adx + dy * dy <= ar * ar) {
          return row;
        }
      }
    }
    return null;
  }

  /**
   * The canvas width this graph needs, so the gutter can be sized to the
   * history instead of clipping deep branches at a fixed width.
   */
  public measureWidth(rows: VisualCommitRow[]): number {
    const { originX, laneWidth, nodeRadius } = this.config;
    let maxLane = 0;
    for (const row of rows) {
      if (row.lane > maxLane) maxLane = row.lane;
      for (const lane of row.active_lanes) {
        if (lane > maxLane) maxLane = lane;
      }
      for (const conn of row.connections) {
        if (conn.to_lane > maxLane) maxLane = conn.to_lane;
      }
    }
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
    const { rowHeight, laneWidth, originX, nodeRadius, mergeNodeRadius, lineWidth } = this.config;
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
    const laneX = (lane: number) => originX + lane * laneWidth;

    const viewportHeight = options.viewportHeight ?? this.canvasHeight(ctx);
    const lookbackStart = Math.max(0, startIndex - LOOKBACK_ROWS);
    // When this pass owns long connectors (any direct render), the window
    // extends EXACTLY to the deepest child that targets a visible row, so an
    // edge of any span draws whole from its own child row — there is no scan
    // cap left to silently drop history behind. Strip painters skip long
    // connectors entirely (the live overlay owns them), so their fixed
    // LOOKBACK priming already covers every edge they draw.
    const renderStart = options.skipLongConnectors
      ? lookbackStart
      : Math.min(
          lookbackStart,
          deepestChildTargetingRange(buildIncomingEdgeIndex(rows), startIndex, endIndex),
        );
    const renderEnd = Math.min(rows.length, endIndex + 5);

    // 1. Pass-through lanes, drawn as one path per unbroken run.
    //
    // Stroking a separate segment per row leaves a visible seam at every row
    // boundary once the line has any transparency or antialiasing, and costs a
    // path per row per lane. A run is one moveTo/lineTo pair however long it is.
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
          if (open && open.colorIndex === colorIndex && Math.abs(open.bottom - yTop) < 0.5) {
            open.bottom = yBottom;
          } else {
            if (open) scratchRuns.push(open);
            scratchOpenRuns.set(lane, acquireRun(lane, colorIndex, yTop, yBottom));
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
        const x = laneX(run.lane);
        ctx.strokeStyle = getBranchColor(run.colorIndex);
        ctx.beginPath();
        ctx.moveTo(x, run.top);
        ctx.lineTo(x, run.bottom);
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
        const xFrom = laneX(row.lane);

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

          const targetRowIdx = i + conn.to_row_offset;
          if (targetRowIdx >= rows.length) continue;

          const yTo = calcY(targetRowIdx);
          const xTo = laneX(conn.to_lane);

          if (Math.max(yFrom, yTo) < -rowHeight || Math.min(yFrom, yTo) > viewportHeight + rowHeight) {
            continue;
          }

          ctx.strokeStyle = getBranchColor(conn.color_index);
          ctx.beginPath();
          if (conn.from_lane === conn.to_lane) {
            ctx.moveTo(xFrom, yFrom);
            ctx.lineTo(xTo, yTo);
          } else if (conn.is_merge) {
            this.cornerAtStart(ctx, xFrom, yFrom, xTo, yTo);
          } else {
            this.cornerAtEnd(ctx, xFrom, yFrom, xTo, yTo);
          }
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
      const x = laneX(row.lane);
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
        const xFrom = this.getLaneX(row.lane);
        for (const conn of row.connections) {
          if (conn.is_dangling) {
            this.strokeDanglingStub(ctx, xFrom, yFrom, rowHeight, conn.color_index);
          }
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
    const { rowHeight, laneWidth, originX, lineWidth } = this.config;
    const theme = options?.theme ?? DEFAULT_THEME;
    const viewHeight = viewportHeight ?? this.canvasHeight(ctx);
    const calcY = this.yForRow(scrollOffset);
    const laneX = (lane: number) => originX + lane * laneWidth;

    const index = buildIncomingEdgeIndex(rows);
    const lo = Math.max(0, startIndex);
    const hi = Math.min(rows.length, Math.max(endIndex, startIndex));

    // Visible target -> child rows reaching it, straight off the index; each
    // child's matching connection is found by scanning its own (tiny) list.
    type LongEdge = { j: number; t: number; xFrom: number; yFrom: number; xTo: number; yTo: number; colorIndex: number };
    const edges: LongEdge[] = [];
    for (let t = lo; t < hi; t++) {
      for (let p = index.starts[t]; p < index.starts[t + 1]; p++) {
        const j = index.children[p];
        if (j >= t) continue;
        const child = rows[j];
        if (!child.connections) continue;
        for (const conn of child.connections) {
          if (conn.is_dangling) continue;
          if (!isLongConnection(conn, LOOKBACK_ROWS)) continue;
          if (j + conn.to_row_offset !== t) continue;
          const yTo = calcY(t);
          const yFrom = calcY(j);
          if (
            Math.max(yFrom, yTo) < -rowHeight ||
            Math.min(yFrom, yTo) > viewHeight + rowHeight
          ) {
            continue;
          }
          edges.push({
            j,
            t,
            xFrom: laneX(child.lane),
            yFrom,
            xTo: laneX(conn.to_lane),
            yTo,
            colorIndex: conn.color_index,
          });
        }
      }
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
        if (e.xFrom === e.xTo) {
          ctx.moveTo(e.xFrom, e.yFrom);
          ctx.lineTo(e.xTo, e.yTo);
        } else {
          // Corner ownership mirrors render(): a merged-in branch peels out
          // just under its child; a closing lane stays straight until arrival.
          const childRow = rows[e.j];
          const isMergeConn = childRow.connections.some(
            (c) =>
              !c.is_dangling &&
              isLongConnection(c, LOOKBACK_ROWS) &&
              e.j + c.to_row_offset === e.t &&
              c.is_merge,
          );
          if (isMergeConn) this.cornerAtStart(ctx, e.xFrom, e.yFrom, e.xTo, e.yTo);
          else this.cornerAtEnd(ctx, e.xFrom, e.yFrom, e.xTo, e.yTo);
        }
        ctx.stroke();
      }

      // Re-stamp every visible node the drawn geometry could overlap:
      // endpoints (the stroke starts and ends on their centres) and any node
      // whose disc sits inside an edge's bounding box.
      const { nodeRadius, mergeNodeRadius } = this.config;
      const pad = Math.max(nodeRadius, mergeNodeRadius) + 2.5 + lineWidth;
      const affected = new Set<number>();
      for (const e of edges) {
        const xMin = Math.min(e.xFrom, e.xTo) - pad;
        const xMax = Math.max(e.xFrom, e.xTo) + pad;
        const yMin = Math.min(e.yFrom, e.yTo) - pad;
        const yMax = Math.max(e.yFrom, e.yTo) + pad;
        for (let t = lo; t < hi; t++) {
          if (t < e.j || t > e.t) continue;
          const x = laneX(rows[t].lane);
          const y = calcY(t);
          if (x < xMin || x > xMax || y < yMin || y > yMax) continue;
          affected.add(t);
        }
      }
      const ordered = [...affected].sort((a, b) => a - b);
      for (const t of ordered) {
        const row = rows[t];
        const x = laneX(row.lane);
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

  /** Corner just below the child: the edge peels out, then runs straight down. */
  private cornerAtStart(
    ctx: CanvasRenderingContext2D,
    xFrom: number,
    yFrom: number,
    xTo: number,
    yTo: number
  ) {
    const radius = this.cornerRadius(yTo - yFrom, xTo - xFrom);
    const yCorner = yFrom + radius;
    ctx.moveTo(xFrom, yFrom);
    ctx.bezierCurveTo(xFrom, yFrom + radius * 0.7, xTo, yCorner - radius * 0.7, xTo, yCorner);
    if (yTo > yCorner) ctx.lineTo(xTo, yTo);
  }

  /** Corner just above the parent: the edge runs straight down, then arrives. */
  private cornerAtEnd(
    ctx: CanvasRenderingContext2D,
    xFrom: number,
    yFrom: number,
    xTo: number,
    yTo: number
  ) {
    const radius = this.cornerRadius(yTo - yFrom, xTo - xFrom);
    const yCorner = yTo - radius;
    ctx.moveTo(xFrom, yFrom);
    if (yCorner > yFrom) ctx.lineTo(xFrom, yCorner);
    ctx.bezierCurveTo(xFrom, yCorner + radius * 0.7, xTo, yTo - radius * 0.7, xTo, yTo);
  }

  /**
   * How wide the turn is: never more than half the vertical span (or the
   * corner would overshoot the parent) and never wider than the lane gap it is
   * crossing (or a one-lane hop would bulge).
   */
  private cornerRadius(deltaY: number, deltaX: number): number {
    const { rowHeight, laneWidth } = this.config;
    const span = Math.max(1, Math.abs(deltaY));
    return Math.max(2, Math.min(rowHeight * 0.85, span / 2, Math.abs(deltaX) || laneWidth));
  }

  private canvasHeight(ctx: CanvasRenderingContext2D): number {
    const height = ctx.canvas?.height;
    if (!height) return 3000;
    const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
    return height / dpr;
  }
}
