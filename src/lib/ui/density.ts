import type { DensityMode } from "../stores/densityStore";

/**
 * Row heights for every fixed-row surface, per density.
 *
 * The Compact/Spacious setting used to reach two surfaces: the branch list and
 * the commit table. The diff, the file tree, blame and coverage all carried
 * their own hard-coded row height and ignored it — so the setting under-
 * delivered on its name in exactly the panes where most of a session is spent,
 * and there was no single place to see what "compact" actually meant.
 *
 * Every value is the surface's previous constant under `spacious`, so turning
 * the setting to Spacious reproduces today's layout exactly and Compact is the
 * only new geometry. The numbers live together because they have to agree
 * about what one step of density is worth; scattered constants is how the
 * commit list ended up 10 px tighter per step than the branch list.
 *
 * These feed VIRTUAL LISTS, whose windowing math positions row n at
 * n * rowHeight. A row that wraps breaks that, so callers that allow wrapping
 * turn virtualization off rather than changing the height here.
 */
export type DensitySurface =
  | "diff"
  | "code"
  | "fileTree"
  | "blame"
  | "coverageFile"
  | "coverageSource";

const ROW_HEIGHTS: Record<DensitySurface, Record<DensityMode, number>> = {
  // Diff and code lines are monospace text at a fixed leading; one step down
  // is the tightest that keeps descenders off the row below.
  diff: { spacious: 20, compact: 17 },
  code: { spacious: 20, compact: 17 },
  // Tree rows carry an icon, so they cannot go as tight as pure text.
  // 26, not the 24 the old padding produced: the redrawn explorer rows
  // carry indent guides and a status mark, and stating the height is what
  // keeps a row inside the slot VirtualList placed it in.
  fileTree: { spacious: 26, compact: 22 },
  blame: { spacious: 24, compact: 20 },
  coverageFile: { spacious: 26, compact: 22 },
  coverageSource: { spacious: 24, compact: 20 },
};

/**
 * Row height in CSS pixels for `surface` at `density`.
 *
 * Falls back to the spacious value for an unrecognized density rather than
 * returning undefined: a NaN row height silently collapses a virtual list to
 * an empty window, which reads as "this file has no content".
 */
export function rowHeight(surface: DensitySurface, density: DensityMode): number {
  const row = ROW_HEIGHTS[surface];
  return row[density] ?? row.spacious;
}

/** Zoom stops the code viewer’s own controls already enforce. */
export const CODE_ZOOM_MIN = 70;
export const CODE_ZOOM_MAX = 160;

/**
 * Pixel height of one code row at `zoomPercent`.
 *
 * The virtual list places row n at n times this value, and the row box has
 * to be exactly that tall. A second leading (the old fixed `leading-5`) drew
 * a 20px line into the 17px compact slot, so neighbouring rows overlapped.
 * Non-finite zoom fails closed to the unscaled height. Anything outside the
 * control range clamps to it, so a hostile value cannot collapse the window
 * or stretch a row across the pane.
 */
export function scaledRowHeight(base: number, zoomPercent: number): number {
  if (!Number.isFinite(base) || base <= 0) return 0;
  const unscaled = Math.max(1, Math.round(base));
  if (!Number.isFinite(zoomPercent)) return unscaled;
  const clamped = Math.min(CODE_ZOOM_MAX, Math.max(CODE_ZOOM_MIN, zoomPercent));
  return Math.max(1, Math.round(base * (clamped / 100)));
}

/** Every surface this module sizes; used by the contract test. */
export const DENSITY_SURFACES = Object.keys(ROW_HEIGHTS) as DensitySurface[];
