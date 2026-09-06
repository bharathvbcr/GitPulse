<script lang="ts">
  /**
   * A bar chart over an already-bucketed commit series.
   *
   * Distinct from `CommitCadence`, which takes raw commits and buckets them
   * itself against a local calendar. These counts arrive pre-bucketed from the
   * backend, in rolling 24-hour spans anchored at the sweep, and re-bucketing
   * them here would file the same commit under two different days depending on
   * which component drew it. Both scale through `barHeights`, so there is one
   * owner of how an empty series looks.
   *
   * Nothing here invents a reading. An absent series renders nothing at all —
   * the caller shows "not scanned" through `FleetCell` — because a flat row of
   * baseline ticks is indistinguishable from a measured quiet stretch.
   */
  import { barHeights, groupBuckets } from "../metrics/commitCadence";

  let {
    counts,
    /** Days the series covers, for the label. */
    windowDays,
    /** Total across the series, so the label agrees with the bars it sits by. */
    total,
    /** True when the underlying walk was capped: the counts are floors. */
    partial = false,
    height = 14,
    /** Pixels per bar. Bars are clipped, never squeezed below one pixel. */
    barWidth = 2,
    /**
     * Most bars to draw. A longer series is *grouped* into this many — summed,
     * never sampled — so the picture still covers the whole window and still
     * totals what the label says. Ninety daily bars in a grid cell measured
     * 260px and pushed the whole table into horizontal scroll; thirteen weekly
     * ones say the same thing in a fifth of the width.
     */
    maxBars = Number.POSITIVE_INFINITY,
    label = "Commits",
  }: {
    counts: readonly number[];
    windowDays: number;
    total: number;
    partial?: boolean;
    height?: number;
    barWidth?: number;
    maxBars?: number;
    label?: string;
  } = $props();

  const bars = $derived(
    Number.isFinite(maxBars) ? groupBuckets(counts, maxBars) : [...counts],
  );
  const peak = $derived(bars.reduce((max, n) => (Number.isFinite(n) && n > max ? n : max), 0));
  const heights = $derived(barHeights(bars, peak));
  // Active days always come off the ORIGINAL daily series. Counting them on
  // the grouped one would report "active in 11 of 90 days" for a repository
  // that committed on eleven separate weeks.
  const activeDays = $derived(counts.reduce((n, c) => (c > 0 ? n + 1 : n), 0));
  const daysPerBar = $derived(bars.length > 0 ? Math.ceil(counts.length / bars.length) : 1);

  const grain = $derived(
    daysPerBar > 1 ? `, one bar per ${daysPerBar} days (busiest ${peak.toLocaleString()})` : `, busiest day ${peak.toLocaleString()}`,
  );

  const description = $derived(
    total === 0
      ? `${label}: none in the last ${windowDays} days`
      : `${label}: ${total.toLocaleString()} across ${activeDays} of the last ${windowDays} days` +
        grain +
        (partial ? " — the history was capped, so these are floors" : ""),
  );
</script>

{#if bars.length > 0}
  <span
    class="inline-flex items-end gap-[1px] shrink-0"
    style="height: {height}px"
    role="img"
    aria-label={description}
    title={description}
    data-testid="fleet-sparkline"
    data-peak={peak}
    data-bars={bars.length}
    data-days-per-bar={daysPerBar}
  >
    {#each heights as fraction, index (index)}
      <span
        class="rounded-t-[1px] {bars[index] > 0 ? 'bg-accent' : 'bg-border/60'}"
        style="width: {barWidth}px; height: {Math.max(1, Math.round(fraction * height))}px"
      ></span>
    {/each}
  </span>
{/if}
