/**
 * The Diff view's History tab: the commit graph in the file rail's column.
 *
 * Moving between commits used to mean leaving for the Graph view. A first
 * answer put the newest forty commits as a text list above the file list,
 * folded by default because two scroll boxes stacked in one narrow column
 * fought over every wheel event. A list cannot show which branch a commit is
 * on, forty is an arbitrary edge, and a folded section is one more click.
 *
 * The History tab replaces it. It takes the rail's whole height (one scroller,
 * never nested), draws the same solved lanes the Graph view draws, pages
 * older history in as it scrolls, and lets two commits be compared.
 *
 * This module holds the decisions so the component stays a renderer: the
 * rail-sized graph geometry, how wide the lane gutter may grow, what a click
 * means while a comparison is being picked, keyboard stepping, and when to
 * load more.
 */
import type { GraphRenderConfig } from "../canvas/GraphRenderer";

export type RailTab = "files" | "history";

/**
 * One history row. The canvas is painted on this pitch and every row is laid
 * out at it, so the two must agree exactly or nodes drift off their rows.
 */
export const HISTORY_ROW_HEIGHT = 26;

/**
 * The Graph view's geometry, narrowed for a ~250px column: 10px lanes instead
 * of 18 fit eight lanes in the room the compact density spends on five.
 */
export const RAIL_GRAPH_CONFIG: GraphRenderConfig = {
  rowHeight: HISTORY_ROW_HEIGHT,
  laneWidth: 10,
  nodeRadius: 3,
  mergeNodeRadius: 4,
  lineWidth: 1.5,
  originX: 9,
};

/** The gutter never takes more than this share of the rail, so text keeps room. */
export const MAX_GUTTER_SHARE = 0.42;
/** The narrowest gutter worth drawing: one lane and its node. */
export const MIN_GUTTER_WIDTH = RAIL_GRAPH_CONFIG.originX * 2 + RAIL_GRAPH_CONFIG.nodeRadius;

export interface GutterLayout {
  /** Pixels the lane gutter occupies. */
  width: number;
  /** True when the graph is wider than the gutter and its right edge is cut. */
  clipped: boolean;
}

/**
 * How wide the lane gutter is for a graph `measured` pixels wide in a rail
 * `railWidth` wide. A wide history is clipped on its right — the lanes
 * furthest from the mainline — and says so, rather than squeezing the commit
 * text to nothing or growing a horizontal scroller inside the vertical one.
 */
export function gutterLayout(measured: number, railWidth: number): GutterLayout {
  const want = Number.isFinite(measured) && measured > 0 ? Math.ceil(measured) : MIN_GUTTER_WIDTH;
  const room = Number.isFinite(railWidth) && railWidth > 0 ? railWidth : 0;
  const cap = Math.max(MIN_GUTTER_WIDTH, Math.floor(room * MAX_GUTTER_SHARE));
  const width = Math.max(MIN_GUTTER_WIDTH, Math.min(want, cap));
  return { width, clipped: want > width };
}

/** What activating a row does, given whether a comparison base is armed. */
export type RowAction =
  | { kind: "open"; commitId: string }
  | { kind: "arm"; commitId: string }
  | { kind: "disarm" }
  | { kind: "compare"; base: string; target: string };

/**
 * Resolves a click (or Enter) on `commitId`.
 *
 * - A plain activation with nothing armed opens the commit.
 * - The compare gesture (Shift+click, Shift+Enter, the row's compare button)
 *   arms that commit as the base; on the armed commit itself it disarms.
 * - Any activation of another commit while a base is armed compares the two.
 *   The base is the older side when both are in the graph, so the diff reads
 *   forward in time whichever order the reader picked them in.
 */
export function resolveRowAction(
  commitId: string,
  armedBase: string | null,
  compareGesture: boolean,
  orderOf: (id: string) => number,
): RowAction {
  if (armedBase === commitId) return compareGesture ? { kind: "disarm" } : { kind: "open", commitId };
  if (armedBase === null) return compareGesture ? { kind: "arm", commitId } : { kind: "open", commitId };
  // Rows are newest first, so a larger index is older.
  const a = orderOf(armedBase);
  const b = orderOf(commitId);
  if (a >= 0 && b >= 0 && b > a) return { kind: "compare", base: commitId, target: armedBase };
  return { kind: "compare", base: armedBase, target: commitId };
}

/** `abc1234..def5678`, the comparison as git would spell it. */
export function compareLabel(base: string, target: string): string {
  return `${base.slice(0, 7)}..${target.slice(0, 7)}`;
}

/**
 * The row a keyboard step lands on: arrows move one, Page keys a screenful,
 * Home/End the ends. Null for a key that is not a step. Clamped, never
 * wrapping: wrapping from the oldest loaded commit to the newest reads as a
 * jump, not as the end of the list.
 */
export function stepRow(current: number, key: string, total: number, pageRows: number): number | null {
  if (total <= 0) return null;
  const page = Math.max(1, Math.floor(pageRows));
  const from = current < 0 ? -1 : Math.min(current, total - 1);
  let next: number;
  switch (key) {
    case "ArrowDown":
      next = from + 1;
      break;
    case "ArrowUp":
      next = from < 0 ? 0 : from - 1;
      break;
    case "PageDown":
      next = from + page;
      break;
    case "PageUp":
      next = from - page;
      break;
    case "Home":
      next = 0;
      break;
    case "End":
      next = total - 1;
      break;
    default:
      return null;
  }
  return Math.max(0, Math.min(total - 1, next));
}

/** Rows from the end at which older history starts loading. */
export const LOAD_MORE_WITHIN_ROWS = 12;

/**
 * Whether the reader has scrolled close enough to the oldest loaded commit
 * that older history should be asked for now, so it lands before they reach
 * the end rather than after.
 */
export function shouldLoadMore(input: {
  scrollTop: number;
  viewportHeight: number;
  total: number;
  rowHeight: number;
  hasMore: boolean;
  loading: boolean;
}): boolean {
  if (!input.hasMore || input.loading || input.total <= 0) return false;
  const bottom = (Number.isFinite(input.scrollTop) ? input.scrollTop : 0) + Math.max(0, input.viewportHeight);
  const lastVisible = Math.floor(bottom / Math.max(1, input.rowHeight));
  return lastVisible >= input.total - LOAD_MORE_WITHIN_ROWS;
}

/**
 * Index of the commit on screen in `ids`, matching an abbreviated id from one
 * source against a full one from another (both ways), with at least seven
 * hex digits so a short prefix cannot match half the history.
 */
export function indexOfCommit(ids: readonly string[], commitId: string | null): number {
  if (!commitId) return -1;
  for (let i = 0; i < ids.length; i++) {
    if (sameCommit(ids[i], commitId)) return i;
  }
  return -1;
}

/**
 * A commit's one-line label. An empty summary is real — `git commit
 * --allow-empty-message` produces one — and a blank row reads as a loading
 * failure.
 */
export function commitLabel(summary: string): string {
  const trimmed = summary.trim();
  return trimmed.length > 0 ? trimmed : "(no commit message)";
}

/** Whether two ids name one commit; see {@link indexOfCommit}. */
export function sameCommit(a: string, b: string | null): boolean {
  if (!b) return false;
  if (a === b) return true;
  const shorter = a.length < b.length ? a : b;
  if (shorter.length < 7) return false;
  return a.startsWith(shorter) && b.startsWith(shorter);
}
