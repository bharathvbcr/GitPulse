<script lang="ts">
  import { graphClipNotice } from "../graph/graphLayout";

  /**
   * Says when the graph canvas ends before the lanes do. The canvas is capped
   * (see `MAX_GRAPH_CONTENT_WIDTH`), so a very wide topology continues off
   * the surface; without this, the cut looked like branches that do not exist.
   */
  let { clippedWidth, laneWidth }: { clippedWidth: number; laneWidth: number } = $props();

  const notice = $derived(graphClipNotice(clippedWidth, laneWidth));
</script>

{#if notice}
  <p
    role="status"
    data-testid="graph-clip-notice"
    class="shrink-0 bg-surface px-2 py-1 text-micro leading-snug text-textMuted"
  >
    {notice}
  </p>
{/if}
