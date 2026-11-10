<script lang="ts">
  import { get } from "svelte/store";
  import { untrack } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { planCommitReconcile, type ReconcileSeen } from "../graph/currentCommit";

  /**
   * Keeps History › Graph's two selections in agreement. Renders nothing.
   *
   * The graph store and the repo store each hold a selection and are written
   * by different paths: the graph store auto-selects the newest commit on open
   * and after a filter drops the selected one, while the repo store (which
   * owns the diff) is only written by a click or `inspectCommitInHistory`.
   * This effect makes the diff follow an auto-selection and the graph follow
   * an inspected commit, so the header, file list, diff and row highlight
   * describe one commit. It plans on id CHANGES only (previous ids live in
   * plain variables, not state), so a watcher refresh that republishes the
   * same ids does nothing and a click is not undone by the repo store's
   * not-yet-updated previous id. Cost: one `cmd_get_commit_diff` read per
   * History open or filter change.
   *
   * It lived inside CommitTable. Graph's reflog list ("HEAD moves") replaces
   * CommitTable on screen, so a History opened straight onto that list — a
   * restored session, or "Open Reflog" from another view — had no reconciler
   * at all, and the details preview read "Reading the diff…" forever. Graph
   * mounts this beside the details pane, under either list.
   */

  const repoSelectedId = $derived($repoStore.selectedCommitId);
  const graphSelectedId = $derived($graphStore.selectedCommit?.id ?? null);
  const repoInRows = $derived(
    repoSelectedId !== null && $graphStore.rows.some((r) => r.id === repoSelectedId),
  );
  let reconcileSeen: ReconcileSeen | null = null;

  $effect(() => {
    // Only the three ids below are tracked; everything else is read untracked
    // so a status poll republishing the repo store never re-runs the plan.
    const repoId = repoSelectedId;
    const graphId = graphSelectedId;
    const inRows = repoInRows;
    untrack(() => {
      const repo = get(repoStore);
      const rows = get(graphStore).rows;
      const plan = planCommitReconcile({
        repoId,
        graphId,
        repoInRows: inRows,
        prev: reconcileSeen,
        foreignSelection: repo.selectionKind !== "commit" && repo.selectedFilePath !== null,
        diff: {
          loaded: repo.selectedDiff !== null,
          pending: repo.selectedDiffPending,
          error: repo.selectedDiffError,
        },
      });
      reconcileSeen = { repoId, graphId, repoInRows: inRows };
      if (plan.selectGraphId !== null) {
        const row = rows.find((r) => r.id === plan.selectGraphId);
        if (row) void graphStore.selectCommit(row);
      }
      if (plan.loadDiffId !== null) void repoStore.selectCommitDiff(plan.loadDiffId);
    });
  });
</script>
