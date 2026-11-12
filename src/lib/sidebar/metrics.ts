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
 * `three-line` is the default: the name owns line one outright, the measured
 * numbers — churn, uncommitted work, files, ahead/behind, freshness — own
 * line two, and the last commit's age and author own line three. The one-line
 * row could not give the name room. Its name was the only flex item in a row
 * of `shrink-0` chips and buttons, so it was the only thing that could give,
 * and it gave all of it: real branches rendered as `b..`, `d..`, `r..`. The
 * two-line row fixed that but packed the numbers and the attribution onto one
 * line, where the author still had to shrink for a busy branch; with its own
 * line the attribution no longer competes with a single number.
 *
 * `two-line` is kept for a reader who wants the denser list with the name
 * still readable: line two carries the numbers and then the age and author.
 *
 * `one-line` is kept because the trade is real — it fits roughly 45% more
 * refs on screen — and a reader who knows their branch names by their first
 * three characters should not be forced to scroll for the privilege.
 *
 * Listed roomiest first, which is the order the Appearance setting shows.
 */
export type BranchRowLayout = "three-line" | "two-line" | "one-line";

export const BRANCH_ROW_LAYOUTS: readonly BranchRowLayout[] = [
  "three-line",
  "two-line",
  "one-line",
];

export const DEFAULT_BRANCH_ROW_LAYOUT: BranchRowLayout = "three-line";

export function isBranchRowLayout(value: unknown): value is BranchRowLayout {
  return value === "three-line" || value === "two-line" || value === "one-line";
}

/**
 * How many lines a layout draws. The one place the layout name becomes a
 * count, so BranchList's markup and `sidebarRowHeight` cannot disagree about
 * how tall a row is: both ask this.
 *
 * An unknown layout is the default's three lines, not one. The preference is
 * validated on read, so this only matters to a caller that bypassed the
 * validator, and a row drawn too tall is untidy where one drawn too short
 * clips its own content and desynchronises the window math.
 */
export function branchRowLines(layout: BranchRowLayout): 1 | 2 | 3 {
  if (layout === "one-line") return 1;
  if (layout === "two-line") return 2;
  return 3;
}

/**
 * Height in px of a two-line branch row, per density.
 *
 * Measured, not chosen round. The row's content stack renders at exactly
 * 28px — two 13px lines (the micro type's 1.3 line height) and a 2px gap —
 * so these are that 28px plus the vertical breathing room each density
 * wants: 6px a side spacious, 3px a side compact. The meta line is 13px, not
 * its 10px of type, because `--text-micro--line-height` is 1.3; the harness
 * holds the 28px so a change to that token fails there. 44px was the first
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
 * Height in px of a three-line branch row, per density.
 *
 * Measured the same way, in `harness/branches.html`: the content stack
 * renders at exactly 47px — three lines of 13px (the 10px micro type's 1.3
 * line height; line one is the name beside the 13px glyph) with a 4px gap
 * between each pair. The padding is the same ~7.5px a side spacious and
 * ~4.5px a side compact the two-line row's comment set out, so 47 + 15 = 62
 * and 47 + 9 = 56. The breathing room is at least the two-line row's: its gap
 * is 2px where this one is 4px, and 62/3 and 56/3 px per line are above the
 * 40/2 and 34/2 it gives each.
 *
 * A harness check ties each of these to the stack it measures, so a change to
 * a line's height or gap fails there instead of clipping a row.
 */
export const BRANCH_ROW_HEIGHT_THREE_LINE: Record<SidebarDensity, number> = {
  spacious: 62,
  compact: 56,
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
 * Only branch rows grow under `two-line` and `three-line`. Section headers,
 * folder headers and tag rows stay exactly as tall as they have always been:
 * a header is one word and a count, and a tag row carries a name and at most
 * one chip, so neither has a second line's worth of anything to say. Giving
 * them one would cost a third of the visible list to whitespace.
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
  const lines = branchRowLines(layout);
  if (kind !== "branch" || lines === 1) {
    return branchRowHeight(density);
  }
  const heights = lines === 2 ? BRANCH_ROW_HEIGHT_TWO_LINE : BRANCH_ROW_HEIGHT_THREE_LINE;
  return heights[density] ?? heights.spacious;
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
