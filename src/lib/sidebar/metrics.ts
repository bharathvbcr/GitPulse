/**
 * Single source of truth for sidebar geometry.
 *
 * BranchList's virtual-window math, its row classes, and app.css's
 * content-visibility hint must agree on the row height or the windowing
 * drifts (blank bands, sticky headers overlapping rows). Both sides read
 * these constants; nothing hardcodes a pixel value anymore. Density comes
 * from densityStore ("spacious" | "compact") — previously the sidebar
 * ignored it entirely and stayed cramped in both modes.
 *
 * Rows are no longer all one height: a two-line branch row is taller than the
 * headers around it, so `sidebarRowHeight` answers per kind and
 * `sidebar/rowWindow.ts` turns those heights into scroll positions.
 */

import type { FlatRow } from "../branches/flattenRows";

export type SidebarDensity = "spacious" | "compact";

/** Row height in px for branch/tag/folder/section header rows, per density. */
export const BRANCH_ROW_HEIGHT: Record<SidebarDensity, number> = {
  spacious: 30,
  compact: 24,
};

export function branchRowHeight(density: SidebarDensity): number {
  return BRANCH_ROW_HEIGHT[density] ?? BRANCH_ROW_HEIGHT.spacious;
}

/* --- Branch row layout ---------------------------------------------------- */

/**
 * How much vertical room one branch gets.
 *
 * `two-line` is the default: the name owns line one outright and every number
 * — churn, ahead/behind, files, author, age — moves to line two. On a
 * default-width sidebar the one-line row could not do this. Its name was the
 * only flex item in a row of `shrink-0` chips and buttons, so it was the only
 * thing that could give, and it gave all of it: real branches rendered as
 * `b..`, `d..`, `r..`. A name the reader cannot read is worse than a number
 * they have to hover for.
 *
 * `one-line` is kept because the trade is real — it fits roughly 45% more
 * refs on screen — and a reader who knows their branch names by their first
 * three characters should not be forced to scroll for the privilege.
 */
export type BranchRowLayout = "two-line" | "one-line";

export const BRANCH_ROW_LAYOUTS: readonly BranchRowLayout[] = ["two-line", "one-line"];

export function isBranchRowLayout(value: unknown): value is BranchRowLayout {
  return value === "two-line" || value === "one-line";
}

/**
 * Height in px of a two-line branch row, per density.
 *
 * Measured, not chosen round. The row's content stack renders at exactly
 * 25px — a 13px icon line beside 12px text, a 2px gap, then 10px of meta —
 * so these are that 25px plus the vertical breathing room each density
 * wants: ~7.5px a side spacious, ~4.5px a side compact. 44px was the first
 * guess and left the pair looking unmoored in the middle of the row.
 *
 * The values live here because BranchList's window math, the row's inline
 * `height`, and `contain-intrinsic-size` all have to agree — that is the
 * whole reason this module exists.
 */
export const BRANCH_ROW_HEIGHT_TWO_LINE: Record<SidebarDensity, number> = {
  spacious: 40,
  compact: 34,
};

/**
 * Kinds of row the sidebar's one shared scroller has to place.
 *
 * Derived from `FlatRow` rather than spelled out again so a new row kind
 * cannot be added to the flattener and silently inherit a height nobody
 * chose for it — the mapping below stops compiling instead.
 */
export type SidebarRowKind = FlatRow["kind"];

/**
 * Height in px of one row, by kind.
 *
 * Only branch rows grow under `two-line`. Section headers, folder headers and
 * tag rows stay exactly as tall as they have always been: a header is one
 * word and a count, and a tag row carries a name and at most one chip, so
 * neither has a second line's worth of anything to say. Giving them one would
 * cost a third of the visible list to whitespace.
 *
 * Unknown density or layout falls back to the roomiest value for the kind,
 * matching `branchRowHeight`: a row drawn too tall is ugly, a row drawn too
 * short clips its own content and desynchronises the window math.
 */
export function sidebarRowHeight(
  kind: SidebarRowKind,
  density: SidebarDensity,
  layout: BranchRowLayout
): number {
  if (kind !== "branch" || layout === "one-line") {
    return branchRowHeight(density);
  }
  return BRANCH_ROW_HEIGHT_TWO_LINE[density] ?? BRANCH_ROW_HEIGHT_TWO_LINE.spacious;
}

/** Rows rendered beyond the visible window so scrolling never shows a gap. */
export const BRANCH_OVERSCAN = 12;

/* --- Sidebar shell sizing ------------------------------------------------- */

export const SIDEBAR_MIN_WIDTH = 264;
export const SIDEBAR_MAX_WIDTH = 560;
export const SIDEBAR_DEFAULT_WIDTH = 360;
/** Collapsed sidebar renders as an icon rail of this width. */
export const SIDEBAR_COLLAPSED_WIDTH = 44;
/** Keyboard resize step for the width separator (ArrowLeft/ArrowRight). */
export const SIDEBAR_RESIZE_STEP = 16;

/**
 * Clamp a requested sidebar width to the supported range. Fail-closed on
 * hostile inputs: NaN/Infinity/non-finite fall back to the default rather
 * than poisoning persisted layout state.
 */
export function clampSidebarWidth(px: number): number {
  if (!Number.isFinite(px)) return SIDEBAR_DEFAULT_WIDTH;
  if (px < SIDEBAR_MIN_WIDTH) return SIDEBAR_MIN_WIDTH;
  if (px > SIDEBAR_MAX_WIDTH) return SIDEBAR_MAX_WIDTH;
  // Snap to whole pixels: fractional widths from sub-pixel drag deltas make
  // the flex layout shimmer between paints.
  return Math.round(px);
}
