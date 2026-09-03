<script lang="ts">
  import type { PulseExtensionChurn } from "../../pulse/types";
  import { Layers } from "lucide-svelte";

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
  <div class="gp-card p-4 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col gap-3">
    <div class="flex items-center gap-2 border-b border-border/50 pb-2.5">
      <Layers size={15} class="text-accent shrink-0" />
      <span class="text-xs font-semibold text-textPrimary uppercase tracking-wider">Churn by extension</span>
      <span class="text-[11px] text-textMuted">From the same numstat walk</span>
    </div>
    <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
      {#each shown as ext (ext.extension)}
        {@const churn = ext.additions + ext.deletions}
        {@const width = maxChurn > 0 ? Math.round((churn / maxChurn) * 100) : 0}
        <div class="flex items-center gap-2 text-xs">
          <span class="w-16 font-mono text-textPrimary truncate" title={ext.extension}>.{ext.extension}</span>
          <div class="flex-1 h-1.5 rounded-full bg-border/40 overflow-hidden">
            <div class="h-full bg-accent/80" style="width: {width}%"></div>
          </div>
          <span class="w-24 text-right font-mono text-textMuted">
            <span class="text-emerald-500">+{ext.additions.toLocaleString()}</span>
            /
            <span class="text-rose-500">-{ext.deletions.toLocaleString()}</span>
          </span>
        </div>
      {/each}
    </div>
  </div>
{/if}
