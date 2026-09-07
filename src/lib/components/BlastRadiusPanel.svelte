<script lang="ts">
  /**
   * Blast radius by hop for a changed-file set (A3).
   *
   * Renders BlastLayer node_count and nodes_omitted — not only the sample —
   * plus unmatched_targets and walk_incomplete. No min_rung control: layered
   * impact cannot combine with a rung filter.
   */
  import type { ComposedBlastRadius } from "../codeintel/blastCompose";
  import { Loader2 } from "@lucide/svelte";

  let {
    blast,
    loading = false,
    title = "Blast radius",
  }: {
    blast: ComposedBlastRadius | null;
    loading?: boolean;
    title?: string;
  } = $props();
</script>

{#if loading}
  <div class="flex items-center gap-1.5 text-[10px] text-textMuted">
    <Loader2 size={11} class="animate-spin" />
    <span>Computing blast radius…</span>
  </div>
{:else if blast}
  <div class="flex flex-col gap-1 rounded-lg border border-border/60 bg-background/60 p-2">
    <div class="flex items-center justify-between gap-2">
      <span class="text-[10px] font-bold uppercase tracking-wider text-textMuted">{title}</span>
      {#if blast.available}
        <span class="font-mono text-[10px] tabular-nums text-textMuted">
          {blast.total_impacted.toLocaleString()} impacted
          {#if blast.overlap_possible}
            <span class="text-amber-500" title="Totals sum per-seed walks; symbols reachable from multiple files may be counted more than once">· may overlap</span>
          {/if}
        </span>
      {/if}
    </div>

    {#if !blast.available}
      <p class="text-[10px] text-amber-500">
        Blast radius unavailable{blast.reason ? `: ${blast.reason}` : ""} — not the same as zero impact.
      </p>
    {:else}
      <ul class="flex flex-col gap-0.5">
        {#each blast.layers as layer (layer.depth)}
          <li class="flex items-baseline gap-2 font-mono text-[10px] text-textSecondary">
            <span class="w-10 shrink-0 text-textMuted">hop {layer.depth}</span>
            <span class="tabular-nums">
              {layer.node_count.toLocaleString()} node{layer.node_count === 1 ? "" : "s"}
            </span>
            {#if layer.nodes_omitted > 0}
              <span class="text-amber-500" title="Sample shows up to 50 nodes; omitted are beyond the sample">
                · {layer.nodes_omitted.toLocaleString()} omitted from sample
              </span>
            {/if}
            {#if layer.nodes.length > 0}
              <span class="min-w-0 truncate text-textMuted" title={layer.nodes.join(", ")}>
                · {layer.nodes.slice(0, 3).join(", ")}{layer.nodes.length > 3 ? "…" : ""}
              </span>
            {/if}
          </li>
        {/each}
      </ul>
      {#if blast.layers.length === 0}
        <p class="text-[10px] text-textMuted">No inbound layers for these seeds.</p>
      {/if}
    {/if}

    {#if blast.unmatched_targets.length > 0}
      <p class="text-[10px] text-amber-500" title={blast.unmatched_targets.join(", ")}>
        Unmatched seeds ({blast.unmatched_targets.length}): {blast.unmatched_targets.slice(0, 4).join(", ")}{blast.unmatched_targets.length > 4 ? "…" : ""}
      </p>
    {/if}
    {#if blast.walk_incomplete}
      <p class="text-[10px] text-amber-500">Walk incomplete: {blast.walk_incomplete}</p>
    {/if}
    {#if blast.layers_truncated}
      <p class="text-[10px] text-amber-500">Layer list truncated by token budget.</p>
    {/if}
    {#if blast.unavailable_seeds.length > 0}
      <p class="text-[10px] text-textMuted" title={blast.unavailable_seeds.map((s) => `${s.seed}: ${s.reason}`).join("\n")}>
        {blast.unavailable_seeds.length} seed(s) refused layered impact
      </p>
    {/if}
  </div>
{/if}
