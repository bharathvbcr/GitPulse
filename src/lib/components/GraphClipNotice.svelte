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
    class="shrink-0 border-t border-border/40 bg-surface/80 px-2 py-1 text-[10px] leading-snug text-textMuted"
  >
    {notice}
  </p>
{/if}
