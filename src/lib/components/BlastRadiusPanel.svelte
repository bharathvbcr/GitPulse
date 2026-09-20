<script lang="ts">
  /**
   * Blast radius for a changed-file set, as one line a reader can act on.
   *
   * This panel used to open expanded, printing five hop rows of fully-qualified
   * `path::Type.member` strings and the kernel's own 60-word essay about
   * unresolved attribution sites — roughly a third of the diff pane spent on
   * chrome, above the code someone opened the pane to read.
   *
   * So the *explanation* now lives behind the disclosure and the *finding*
   * sits on the summary row. What did NOT move is the qualification: the
   * headline states whether the number is exact, a floor, a ceiling, or merely
   * approximate, because a bounded answer wearing an exact answer's clothes is
   * the failure the honesty rule exists to prevent. Collapsing detail is fine;
   * collapsing a caveat is not.
   *
   * Amber is reserved for answers that are degraded — interrupted or
   * unavailable. A lower bound is the ordinary case on any real repository,
   * and colouring the ordinary case as a warning is what teaches readers to
   * stop seeing the warnings that matter.
   */
  import { isCancelledReason, type ComposedBlastRadius } from "../codeintel/blastCompose";
  import { boundedJoin, tooltipWalkIncomplete } from "../codeintel/walkIncomplete";
  import { blastGlance, confidenceLabel } from "../codeintel/blastGlance";
  import { nodeLabel } from "../codeintel/nodeLabel";
  import { ChevronDown, ChevronRight, Loader2 } from "@lucide/svelte";

  let {
    blast,
    loading = false,
    title = "Blast radius",
    open = $bindable(false),
  }: {
    blast: ComposedBlastRadius | null;
    loading?: boolean;
    title?: string;
    /** Bindable so a host can remember the reader's choice across files. */
    open?: boolean;
  } = $props();

  const glance = $derived(blastGlance(blast));

  /** Degraded, not merely bounded. Only these earn amber. */
  const degraded = $derived(
    glance.confidence === "unavailable" || glance.confidence === "partial",
  );

  const refusedSeeds = $derived(
    blast ? blast.unavailable_seeds.filter((s) => !isCancelledReason(s.reason)) : [],
  );
  const cancelledSeeds = $derived(
    blast ? blast.unavailable_seeds.filter((s) => isCancelledReason(s.reason)) : [],
  );

  const summaryTitle = $derived(
    [glance.headline, ...glance.caveats].join(" · "),
  );
</script>

{#if loading}
  <div
    class="flex items-center gap-1.5 rounded-lg border border-border/50 bg-background/50 px-2.5 py-1.5 text-[11px] text-textMuted"
  >
    <Loader2 size={12} class="animate-spin" />
    <span>Measuring what this change reaches…</span>
  </div>
{:else if blast}
  <div
    class="overflow-hidden rounded-lg border bg-background/50 {degraded
      ? 'border-amber-500/40'
      : 'border-border/60'}"
  >
    <!-- The summary row is the whole panel for most readers. -->
    <button
      type="button"
      class="flex w-full items-center gap-2 px-2.5 py-1.5 text-left transition-colors hover:bg-surfaceHover/60"
      aria-expanded={open}
      onclick={() => (open = !open)}
      title={summaryTitle}
    >
      <span
        class="size-1.5 shrink-0 rounded-full {degraded
          ? 'bg-amber-500'
          : glance.confidence === 'exact'
            ? 'bg-emerald-500'
            : 'bg-accent/70'}"
        aria-hidden="true"
      ></span>
      <span class="min-w-0 flex-1 truncate text-[11px] text-textPrimary">
        {glance.headline}
      </span>
      {#if glance.qualified}
        <span
          class="shrink-0 rounded-full border px-1.5 text-[9px] uppercase tracking-wide {degraded
            ? 'border-amber-500/40 text-amber-600 dark:text-amber-400'
            : 'border-border/70 text-textMuted'}"
        >
          {confidenceLabel(glance.confidence)}
        </span>
      {/if}
      {#if open}
        <ChevronDown size={13} class="shrink-0 text-textMuted" />
      {:else}
        <ChevronRight size={13} class="shrink-0 text-textMuted" />
      {/if}
    </button>

    {#if open}
      <div class="flex flex-col gap-2 border-t border-border/50 px-2.5 py-2">
        <div class="flex items-baseline justify-between gap-2">
          <span class="text-[9px] font-bold uppercase tracking-wider text-textMuted">{title}</span>
          {#if blast.available}
            <span class="font-mono text-[10px] tabular-nums text-textMuted">
              {blast.total_impacted.toLocaleString()} impacted
            </span>
          {/if}
        </div>

        {#if blast.available}
          {#if blast.layers.length > 0}
            <ul class="flex flex-col gap-1">
              {#each blast.layers as layer (layer.depth)}
                <li class="flex items-baseline gap-2 text-[10px]">
                  <span class="w-9 shrink-0 font-mono text-textMuted">hop {layer.depth}</span>
                  <span class="w-14 shrink-0 font-mono tabular-nums text-textSecondary">
                    {layer.node_count.toLocaleString()}
                  </span>
                  <!-- Symbol first: the name answers "what breaks?", and it is
                       what the right-hand clip used to eat. -->
                  <span class="flex min-w-0 flex-1 flex-wrap items-baseline gap-x-2 gap-y-0.5">
                    {#each layer.nodes.slice(0, 3) as node (node)}
                      {@const label = nodeLabel(node)}
                      <span class="min-w-0 truncate" title={label.path ?? node}>
                        <span class="font-mono text-textSecondary">{label.symbol}</span>
                        {#if label.file}
                          <span class="text-textMuted/70">{label.file}</span>
                        {/if}
                      </span>
                    {/each}
                    {#if layer.nodes.length > 3}
                      <span class="text-textMuted" title={boundedJoin(layer.nodes, 8)}>
                        +{layer.nodes.length - 3} more shown
                      </span>
                    {/if}
                    {#if layer.nodes_omitted > 0}
                      <span
                        class="text-textMuted"
                        title="The engine returns a capped sample of each band; these are beyond it"
                      >
                        {layer.nodes_omitted.toLocaleString()} not sampled
                      </span>
                    {/if}
                  </span>
                </li>
              {/each}
            </ul>
          {:else}
            <p class="text-[10px] text-textMuted">No inbound layers for these seeds.</p>
          {/if}
        {/if}

        {#if glance.caveats.length > 0}
          <ul class="flex flex-col gap-0.5 border-t border-border/40 pt-1.5">
            {#each glance.caveats as caveat (caveat)}
              <li class="flex gap-1.5 text-[10px] {degraded ? 'text-amber-600 dark:text-amber-400' : 'text-textMuted'}">
                <span aria-hidden="true">·</span>
                <span class="min-w-0">{caveat}</span>
              </li>
            {/each}
          </ul>
        {/if}

        {#if blast.unmatched_targets.length > 0}
          <p class="text-[10px] text-textMuted" title={boundedJoin(blast.unmatched_targets, 12)}>
            Not in the index: {blast.unmatched_targets.slice(0, 4).join(", ")}{blast
              .unmatched_targets.length > 4
              ? "…"
              : ""}
          </p>
        {/if}

        {#if cancelledSeeds.length > 0}
          <p
            class="text-[10px] text-amber-600 dark:text-amber-400"
            title={boundedJoin(cancelledSeeds.map((s) => s.seed), 12)}
          >
            {cancelledSeeds.length} file(s) cancelled before the walk finished — the hops above are a
            partial answer, not an unindexed map.
          </p>
        {/if}

        {#if refusedSeeds.length > 0}
          <p
            class="text-[10px] text-textMuted"
            title={boundedJoin(refusedSeeds.map((s) => `${s.seed}: ${s.reason}`), 8)}
          >
            {refusedSeeds.length} file(s) refused layered impact
          </p>
        {/if}

        <!-- The kernel's verbatim account. Folded, never dropped: the reader
             who wants to know exactly what was missed can still find out. -->
        {#if glance.detail}
          {@const detailTitle = tooltipWalkIncomplete([blast.walk_incomplete, blast.reason])}
          <details class="border-t border-border/40 pt-1.5">
            <summary
              class="cursor-pointer text-[10px] text-textMuted hover:text-textPrimary"
              title={detailTitle}
            >
              What the engine reported
            </summary>
            <p class="mt-1 font-mono text-[10px] leading-relaxed text-textMuted">
              {glance.detail}
            </p>
          </details>
        {/if}
      </div>
    {/if}
  </div>
{/if}
