import {
  resolveOverflowHint,
  type OverflowHint,
} from "../dom/overflowHint";

export type GraphWidthMode = "balanced" | "wide" | "full";

/** Fallback canvas width when the measured lane width is not a number. */
export const MIN_GRAPH_CONTENT_WIDTH = 220;

/**
 * Browser canvas dimensions and backing-store memory are finite. Keeping the
 * CSS surface below this ceiling also bounds a 2x DPR backing store to 8192px.
 * Topologies wider than the surface remain honest: their lanes continue off
 * canvas instead of making the commit list or the renderer allocation unbound.
 */
export const MAX_GRAPH_CONTENT_WIDTH = 4_096;

/** Commit metadata reserve on ordinary/wide panes. */
export const COMMIT_PANE_RESERVE = 480;

const MODE_LIMITS: Record<GraphWidthMode, number> = {
  balanced: 440,
  wide: 720,
  full: Number.POSITIVE_INFINITY,
};

export interface GraphLayoutInput {
  measuredLaneWidth: number;
  avatarSlotWidth: number;
  availableWidth: number;
  widthMode: GraphWidthMode;
}

export interface GraphLayout {
  availableWidth: number;
  /** Width of the horizontally scrollable canvas surface. */
  contentWidth: number;
  /** Width the graph is allowed to consume in the two-pane layout. */
  viewportWidth: number;
  /** Width left for hashes, summaries, refs, authors and dates. */
  remainingWidth: number;
  isHorizontallyScrollable: boolean;
  /**
   * Width of lane area that lies beyond [`MAX_GRAPH_CONTENT_WIDTH`] and is
   * therefore not drawn; 0 when the whole topology fits.
   */
  clippedWidth: number;
}

/**
 * The sentence for lanes the canvas cannot draw, or `null` when none are
 * cut. A graph wider than the surface used to end at the ceiling with no
 * mark, so the missing branches looked like branches that do not exist.
 */
export function graphClipNotice(clippedWidth: number, laneWidth: number): string | null {
  if (!(clippedWidth > 0)) return null;
  const lanes = laneWidth > 0 ? Math.max(1, Math.ceil(clippedWidth / laneWidth)) : null;
  const limit = MAX_GRAPH_CONTENT_WIDTH.toLocaleString("en-US");
  return lanes === null
    ? `Some branch lanes extend past the ${limit} px the graph can draw and are not shown.`
    : `${lanes} branch lane${lanes === 1 ? "" : "s"} extend${lanes === 1 ? "s" : ""} past the ${limit} px the graph can draw and ${lanes === 1 ? "is" : "are"} not shown.`;
}

/** Graph gutter overflow; same shape as the shared overflow hint. */
export type GraphOverflowHint = OverflowHint;

export function isGraphWidthMode(value: unknown): value is GraphWidthMode {
  return value === "balanced" || value === "wide" || value === "full";
}

function finiteNonNegative(value: number, fallback: number): number {
  return Number.isFinite(value) && value >= 0 ? value : fallback;
}

/**
 * Resolves graph geometry without allowing repository topology to own the
 * entire view. The natural canvas stays horizontally scrollable, while the
 * viewport is capped by both the user's preference and a non-negotiable
 * commit-row reserve. On narrow panes the reserve scales to 55%, so neither
 * side can push the other completely off-screen.
 */
export function resolveGraphLayout(input: GraphLayoutInput): GraphLayout {
  const availableWidth = finiteNonNegative(input.availableWidth, 0);
  const measuredLaneWidth = finiteNonNegative(
    input.measuredLaneWidth,
    MIN_GRAPH_CONTENT_WIDTH,
  );
  const avatarSlotWidth = finiteNonNegative(input.avatarSlotWidth, 0);
  const naturalWidth = measuredLaneWidth + avatarSlotWidth;
  const contentWidth = Math.min(MAX_GRAPH_CONTENT_WIDTH, naturalWidth);
  const clippedWidth = Math.max(0, naturalWidth - contentWidth);

  if (availableWidth === 0) {
    return {
      availableWidth,
      contentWidth,
      viewportWidth: 0,
      remainingWidth: 0,
      isHorizontallyScrollable: contentWidth > 0,
      clippedWidth,
    };
  }

  const commitReserve = Math.min(COMMIT_PANE_RESERVE, availableWidth * 0.55);
  const hardViewportLimit = Math.max(0, availableWidth - commitReserve);
  // The store validates persisted values, but this helper is also an IPC/UI
  // boundary: an unexpected mode must degrade to the safe default rather than
  // letting Math.min(undefined, ...) poison the entire layout with NaN.
  const preferredLimit = MODE_LIMITS[input.widthMode] ?? MODE_LIMITS.balanced;
  const viewportWidth = Math.min(
    contentWidth,
    availableWidth,
    hardViewportLimit,
    preferredLimit,
  );
  const remainingWidth = Math.max(0, availableWidth - viewportWidth);

  return {
    availableWidth,
    contentWidth,
    viewportWidth,
    remainingWidth,
    isHorizontallyScrollable: contentWidth > viewportWidth + 0.5,
    clippedWidth,
  };
}

/**
 * Flex-item contract for the graph gutter. Extra branch lanes must overflow
 * inside this box: `min-width:auto` would size the column to the canvas
 * content width and shove the commit list off-screen.
 */
export function graphViewportBoxStyle(viewportWidth: number): string {
  const w = finiteNonNegative(viewportWidth, 0);
  return `width:${w}px;max-width:${w}px;min-width:0px;flex-basis:${w}px;`;
}

/** Inner canvas surface: wide enough that overflow-x on the gutter can pan. */
export function graphContentBoxStyle(contentWidth: number): string {
  const w = finiteNonNegative(contentWidth, 0);
  return `width:${w}px;min-width:${w}px;height:100%;`;
}

export function resolveGraphOverflow(
  scrollLeft: number,
  viewportWidth: number,
  contentWidth: number,
): GraphOverflowHint {
  return resolveOverflowHint(scrollLeft, viewportWidth, contentWidth);
}

export function clampGraphScrollLeft(
  scrollLeft: number,
  viewportWidth: number,
  contentWidth: number,
): number {
  const max = Math.max(
    0,
    finiteNonNegative(contentWidth, 0) - finiteNonNegative(viewportWidth, 0),
  );
  const left = Number.isFinite(scrollLeft) ? scrollLeft : 0;
  return Math.min(max, Math.max(0, left));
}
