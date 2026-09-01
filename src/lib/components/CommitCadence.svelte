<script lang="ts">
  import {
    activeDayCount,
    bucketCommitsByDay,
    sparklineHeights,
    type CadenceCommit,
  } from "../metrics/commitCadence";

  let {
    commits = [],
    days = 30,
    now = Date.now(),
  }: {
    commits?: readonly CadenceCommit[];
    days?: number;
    /** Injected so the rendering is deterministic under test. */
    now?: number;
  } = $props();

  const summary = $derived(bucketCommitsByDay(commits, days, now));
  const heights = $derived(sparklineHeights(summary));
  const active = $derived(activeDayCount(summary));

  // A repository with fewer commits than buckets still renders a full axis;
  // the label says the span is partial rather than implying a quiet stretch
  // that the history simply does not cover.
  const label = $derived(
    summary.total === 0
      ? `No commits in the last ${summary.buckets.length} days`
      : `${summary.total} commit${summary.total === 1 ? "" : "s"} across ${active} of the last ` +
        `${summary.buckets.length} days` +
        (summary.partial ? ", which is the whole loaded history" : ""),
  );
</script>

{#if summary.buckets.length > 0}
  <span
    class="inline-flex items-end gap-[1px] h-3 shrink-0"
    role="img"
    aria-label={label}
    title={label}
  >
    {#each summary.buckets as bucket, index (bucket.day)}
      <span
        class="w-[2px] rounded-t-[1px] {bucket.count > 0 ? 'bg-accent' : 'bg-border/70'}"
        style="height: {Math.max(1, Math.round(heights[index] * 12))}px"
      ></span>
    {/each}
  </span>
{/if}
