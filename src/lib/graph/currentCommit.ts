/**
 * One owner for "which commit is the History view on".
 *
 * Two stores each hold a selection: the graph store (`selectedCommit`, which
 * drives the header, the file list and the ring on the canvas) and the repo
 * store (`selectedCommitId`, which owns the diff). They are written by
 * different paths, so on a fresh open, after a filter drops the clicked commit,
 * or after `inspectCommitInHistory`, they name different commits (or one names
 * none) and the three panes disagree. Every consumer asks this module instead
 * of reading either id directly.
 */

/**
 * The commit every pane should describe: the repo store's selection when that
 * commit is in the loaded rows, otherwise the graph store's. A repo selection
 * that is not in the rows (a filter excluded it, or the page is shorter) names
 * a commit this view cannot show, so it never wins.
 */
export function resolveCurrentCommitId(
  repoSelectedId: string | null,
  graphSelectedId: string | null,
  rows: readonly { id: string }[],
): string | null {
  if (repoSelectedId !== null && rows.some((row) => row.id === repoSelectedId)) {
    return repoSelectedId;
  }
  return graphSelectedId;
}

/**
 * Where the commit list should scroll when a new filter's rows land.
 *
 * A user scrolled deep into history who types a search must not stay mid-list
 * (or pinned to the bottom) of a shorter result and conclude it found little.
 * The top is the answer, unless the commit the panes are on survived the
 * filter and is below the first screenful: that one is brought into view
 * (centred), because losing it would leave the panes describing a row nobody
 * can see.
 */
export function scrollTopForNewFilter(input: {
  rows: readonly { id: string }[];
  currentId: string | null;
  rowHeight: number;
  viewportHeight: number;
}): number {
  const { rows, currentId, rowHeight, viewportHeight } = input;
  if (currentId === null || !(rowHeight > 0)) return 0;
  const index = rows.findIndex((row) => row.id === currentId);
  if (index < 0) return 0;
  const viewport = Number.isFinite(viewportHeight) && viewportHeight > 0 ? viewportHeight : 0;
  const top = index * rowHeight;
  if (top + rowHeight <= viewport) return 0;
  const max = Math.max(0, rows.length * rowHeight - viewport);
  return Math.min(max, Math.max(0, top - Math.max(0, viewport - rowHeight) / 2));
}

/** What the reconciler saw last time; `null` before its first run. */
export interface ReconcileSeen {
  repoId: string | null;
  graphId: string | null;
  repoInRows: boolean;
}

export interface ReconcileInput {
  repoId: string | null;
  graphId: string | null;
  /** Whether `repoId` is one of the loaded rows. */
  repoInRows: boolean;
  /** What the previous run saw; `null` on the first run, which counts as a change. */
  prev: ReconcileSeen | null;
  /**
   * True when the repo store holds a selection that is not a commit (a
   * working-tree file or a range). The reconciler must not replace it with a
   * commit diff just because History mounted.
   */
  foreignSelection: boolean;
  diff: { loaded: boolean; pending: boolean; error: string | null };
}

export interface ReconcilePlan {
  /** Make the graph store select this row (details, ring, header follow). */
  selectGraphId: string | null;
  /** Make the repo store read this commit's diff. */
  loadDiffId: string | null;
}

const NOTHING: ReconcilePlan = { selectGraphId: null, loadDiffId: null };

/**
 * Decides what to write so the two selections agree. Keyed on CHANGES to the
 * ids (and to whether the repo's id is still in the rows): a watcher refresh
 * that republishes the same ids plans nothing, and a click (which writes the
 * graph store first and the repo store when the diff lands) is never undone by
 * the repo store's not-yet-updated previous id.
 *
 * Loop guards: no diff read is planned while one is pending, and none for an
 * id whose last read failed (a failed read leaves `loaded` false and would
 * otherwise re-fire forever).
 */
export function planCommitReconcile(input: ReconcileInput): ReconcilePlan {
  const { repoId, graphId, repoInRows, prev, diff } = input;
  const repoChanged = prev === null || prev.repoId !== repoId || prev.repoInRows !== repoInRows;
  const graphChanged = prev === null || prev.graphId !== graphId;

  if (repoId !== null && repoInRows) {
    // The user's own intent (a diff opened elsewhere, inspectCommitInHistory):
    // the graph follows it, and the diff is read if nobody has read it yet.
    if (!repoChanged) return NOTHING;
    return {
      selectGraphId: graphId !== repoId ? repoId : null,
      loadDiffId: !diff.loaded && !diff.pending && diff.error === null ? repoId : null,
    };
  }

  // The repo store names no commit this view shows: the graph store's
  // (auto-selected) commit is the one on screen, so the diff follows it.
  if (graphId === null || (!repoChanged && !graphChanged)) return NOTHING;
  if (repoId === null && input.foreignSelection) return NOTHING;
  if (diff.pending) return NOTHING;
  if (diff.error !== null && repoId === graphId) return NOTHING;
  return { selectGraphId: null, loadDiffId: graphId };
}
