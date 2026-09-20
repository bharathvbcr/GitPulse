<script lang="ts">
  /**
   * min_rung control + RungHistogram "what you did not see" line (A4).
   *
   * Hidden when layered impact is active — the kernel refuses that pairing.
   *
   * The histogram used to render as a free `<p>` beneath the select. Inside
   * the diff header — a single flex row — an unbounded paragraph wraps to two
   * lines and stretches every sibling in the row with it, which is what made
   * that header look broken at ordinary widths. It is the same sentence
   * either way, so it moves to the control's own tooltip and a compact count
   * that cannot reflow, rather than being dropped.
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

  /**
   * How many results the current filter is holding back.
   *
   * A number is what makes the filter's effect legible at a glance; the
   * sentence behind it stays one hover away.
   */
  const filteredOut = $derived(histogram?.filtered_out ?? 0);
</script>

{#if visible}
  <label
    class="flex shrink-0 items-center gap-1.5 whitespace-nowrap text-[10px] text-textMuted"
    title={histLine ?? undefined}
  >
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
    {#if filteredOut > 0}
      <span class="shrink-0 tabular-nums text-textMuted/80">
        −{filteredOut.toLocaleString()}
      </span>
    {/if}
  </label>
{/if}
