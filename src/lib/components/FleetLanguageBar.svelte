<script lang="ts">
  /**
   * A language mix as a proportional strip.
   *
   * Purely presentational: it takes segments and draws them. `LanguageSegment`
   * is the status bar's own control — it subscribes to `locMetric` for the
   * active repository and owns a popover — and could not be pointed at a fleet
   * total without dragging that coupling along. What the two share is the
   * *fold*, `pickLanguageBarStats`, which both go through; the strip itself is
   * six lines of markup and duplicating those is cheaper than a component that
   * has to know whether it belongs to one repository or twenty.
   *
   * The one rule it enforces: segments never fill a bar they do not account
   * for. A mix whose percentages sum to less than a hundred leaves the
   * remainder as visible track, so a partial reading looks partial.
   */
  import type { LanguageStat } from "../language/barStats";

  let {
    stats,
    height = 6,
    /** Shown as the strip's accessible description. */
    label = "Language mix",
  }: {
    stats: readonly LanguageStat[];
    height?: number;
    label?: string;
  } = $props();

  const widths = $derived(
    stats.map((stat) => (Number.isFinite(stat.percentage) ? Math.max(0, stat.percentage) : 0)),
  );
  const accounted = $derived(widths.reduce((sum, width) => sum + width, 0));
  const description = $derived(
    stats.length === 0
      ? `${label}: not recorded`
      : `${label}: ` +
        stats.map((stat, i) => `${stat.language} ${widths[i].toFixed(1)}%`).join(", ") +
        (accounted < 99.5 ? ` — ${(100 - accounted).toFixed(1)}% unaccounted for` : ""),
  );
</script>

{#if stats.length > 0}
  <span
    class="flex rounded-full overflow-hidden bg-background ring-1 ring-border/50 w-full"
    style="height: {height}px"
    role="img"
    aria-label={description}
    title={description}
    data-testid="fleet-language-bar"
    data-accounted={accounted.toFixed(1)}
  >
    {#each stats as stat, index (stat.language)}
      <span style="width: {widths[index]}%; background-color: {stat.color_hex};"></span>
    {/each}
  </span>
{/if}
