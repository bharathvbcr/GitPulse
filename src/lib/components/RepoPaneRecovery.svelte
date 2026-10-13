<script lang="ts">
  import { currentRepoPath } from "../stores/repoStore";
  import { repoScope } from "../repos/repoScope.svelte";

  /**
   * Rendered inside a crashed pane's failure view, and draws nothing. It hands
   * the pane back to its boundary when the repository changes.
   *
   * A pane that breaks on one repository's data (a status row it cannot draw)
   * used to heal on the next tab switch, because the whole pane was rebuilt
   * under a `{#key currentPath}`. It is no longer rebuilt, so the recovery the
   * rebuild gave for free is spelled out here. The reset is deferred a
   * microtask: a boundary resets by re-running its children, which is not
   * something to start from inside the effect flush that reported the change.
   */
  let { reset }: { reset: () => void } = $props();

  repoScope(
    () => $currentRepoPath,
    () => queueMicrotask(reset),
  );
</script>
