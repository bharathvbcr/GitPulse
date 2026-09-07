<script lang="ts">
  /**
   * min_rung control + RungHistogram "what you did not see" line (A4).
   *
   * Hidden when layered impact is active — the kernel refuses that pairing.
   */
  import {
    RUNG_OPTIONS,
    rungHistogramLine,
    shouldShowRungControl,
  } from "../codeintel/rungFilter";
  import type { CodeintelRung, CodeintelRungHistogram } from "../codeintel/types";

  let {
    minRung = $bindable<"all" | CodeintelRung>("all"),
    histogram = null,
    layeredImpactActive = false,
  }: {
    minRung?: "all" | CodeintelRung;
    histogram?: CodeintelRungHistogram | null;
    layeredImpactActive?: boolean;
  } = $props();

  const visible = $derived(shouldShowRungControl(layeredImpactActive));
  const histLine = $derived(rungHistogramLine(histogram));
</script>

{#if visible}
  <div class="flex flex-col gap-0.5">
    <label class="flex items-center gap-1.5 text-[10px] text-textMuted">
      <span class="shrink-0">min rung</span>
      <select
        class="rounded border border-border/70 bg-background px-1 py-0.5 font-mono text-[10px] text-textPrimary"
        bind:value={minRung}
        aria-label="Minimum resolution rung for impact"
      >
        {#each RUNG_OPTIONS as opt (opt.value)}
          <option value={opt.value}>{opt.label}</option>
        {/each}
      </select>
    </label>
    {#if histLine}
      <p class="text-[9px] leading-snug text-textMuted" title={histLine}>{histLine}</p>
    {/if}
  </div>
{/if}
