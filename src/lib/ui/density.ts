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
  fileTree: { spacious: 24, compact: 20 },
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

/** Every surface this module sizes; used by the contract test. */
export const DENSITY_SURFACES = Object.keys(ROW_HEIGHTS) as DensitySurface[];
