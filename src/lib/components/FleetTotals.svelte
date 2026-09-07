<script module lang="ts">
  import type { FleetTally } from "../fleet/aggregate";

  /** One reading on the totals strip. */
  export interface FleetTotal {
    readonly key: string;
    readonly label: string;
    /** Already formatted in the reading's own units. */
    readonly text: string;
    /** The tally behind it, or null for a reading that is simply a count of rows. */
    readonly tally: FleetTally | null;
    /** Extra context for a reading with no tally (e.g. "3 need attention"). */
    readonly note?: string;
  }
</script>

<script lang="ts">
  /**
   * The workspace's headline readings, as one dense strip.
   *
   * This was five cards in a grid — 62 pixels on a wide window and 124 on a
   * narrow one, above a Pulse panel and a toolbar, before a single repository
   * appeared. A third of an 800-pixel window spent on summary before any of
   * the thing being summarised.
   *
   * Compressing it did not cost the honesty rule, because the rule was being
   * applied wastefully: every tile printed "across all open repositories" even
   * when the total really did cover everything. `describeTally` already
   * returns "" for exactly that case, so the clause now appears **only when
   * there is a shortfall** — and when it appears it is amber and inline, which
   * is louder than the grey caption it replaces, not quieter.
   */
  import { describeTally, isComplete } from "../fleet/aggregate";

  let { totals }: { totals: readonly FleetTotal[] } = $props();

  /** The clause for a reading, or "" when the number really does stand alone. */
  function clauseOf(total: FleetTotal): string {
    if (total.tally === null) return total.note ?? "";
    return describeTally(total.tally);
  }

  /** True when a reading is a complete measurement of every repository. */
  function complete(total: FleetTotal): boolean {
    return total.tally === null || isComplete(total.tally);
  }
</script>

<div
  class="flex flex-wrap items-baseline gap-x-4 gap-y-1 text-[11px]"
  data-testid="fleet-totals"
>
  {#each totals as total (total.key)}
    {@const clause = clauseOf(total)}
    <span class="inline-flex items-baseline gap-1.5 min-w-0">
      <span class="text-[10px] uppercase tracking-wider text-textMuted">{total.label}</span>
      <span class="font-semibold text-textPrimary tabular-nums">{total.text}</span>
      {#if clause && !complete(total)}
        <!-- A number that could not cover everything never travels alone. The
             clause is amber and inline rather than a grey caption, because it
             is the part that changes what the number means. -->
        <span
          class="text-[10px] text-amber-600 dark:text-amber-400 truncate max-w-[16rem]"
          title={clause}
          data-testid="fleet-total-shortfall">{clause}</span
        >
      {:else if clause}
        <span class="text-[10px] text-textMuted truncate max-w-48" title={clause}>
          {clause}
        </span>
      {/if}
    </span>
  {/each}
</div>
