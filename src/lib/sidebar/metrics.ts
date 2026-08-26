/**
 * Single source of truth for sidebar geometry.
 *
 * BranchList's virtual-window math, its row classes, and app.css's
 * content-visibility hint must agree on the row height or the windowing
 * drifts (blank bands, sticky headers overlapping rows). Both sides read
 * these constants; nothing hardcodes a pixel value anymore. Density comes
 * from densityStore ("spacious" | "compact") — previously the sidebar
 * ignored it entirely and stayed cramped in both modes.
 */

export type SidebarDensity = "spacious" | "compact";

/** Row height in px for branch/tag/folder/section header rows, per density. */
export const BRANCH_ROW_HEIGHT: Record<SidebarDensity, number> = {
  spacious: 30,
  compact: 24,
};

export function branchRowHeight(density: SidebarDensity): number {
  return BRANCH_ROW_HEIGHT[density] ?? BRANCH_ROW_HEIGHT.spacious;
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
