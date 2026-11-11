<script lang="ts">
  import type { PulseExtensionChurn } from "../../pulse/types";
  import { Layers } from "@lucide/svelte";

  let {
    extensions = [],
  }: {
    extensions: readonly PulseExtensionChurn[];
  } = $props();

  const maxChurn = $derived(
    extensions.reduce((m, e) => Math.max(m, e.additions + e.deletions), 0),
  );
  const shown = $derived(extensions.slice(0, 12));
</script>

{#if shown.length > 0}
  <div class="gp-card p-4 flex flex-col gap-3">
    <div class="flex items-center gap-2">
      <Layers size={15} class="text-accent shrink-0" />
      <span class="text-xs font-semibold text-textPrimary">Churn by extension</span>
      <span class="text-meta text-textMuted">From the same numstat walk</span>
      {#if extensions.length > shown.length}
        <!-- A cut list must not read as the whole repository: say how many
             extensions the card is not showing. -->
        <span class="gp-chip ml-auto" data-tone="neutral">Top {shown.length} of {extensions.length}</span>
      {/if}
    </div>
    <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
      {#each shown as ext (ext.extension)}
        {@const churn = ext.additions + ext.deletions}
        {@const width = maxChurn > 0 ? Math.round((churn / maxChurn) * 100) : 0}
        <div class="flex items-center gap-2 text-xs">
          <span class="w-16 font-mono text-textPrimary truncate" title={ext.extension}>.{ext.extension}</span>
          <div class="flex-1 h-1.5 rounded-full bg-surfaceHover overflow-hidden">
            <div class="h-full bg-accent/80" style="width: {width}%"></div>
          </div>
          <span class="w-24 text-right font-mono text-textMuted">
            <span class="text-ok">+{ext.additions.toLocaleString()}</span>
            /
            <span class="text-danger">-{ext.deletions.toLocaleString()}</span>
          </span>
        </div>
      {/each}
    </div>
  </div>
{/if}
