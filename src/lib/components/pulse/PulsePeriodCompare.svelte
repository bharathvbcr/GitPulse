<script lang="ts">
  import { computePeriodCompare } from "../../pulse/metrics";
  import type { PulseCommitSummary } from "../../pulse/types";
  import { ArrowUpRight, ArrowDownRight, Minus, TrendingUp } from "@lucide/svelte";

  let {
    commits = [],
    truncated = false,
  }: {
    commits: readonly PulseCommitSummary[];
    /** The scan stopped before the start of history. */
    truncated?: boolean;
  } = $props();

  const deltas = $derived(computePeriodCompare(commits, Date.now(), 30, truncated));

  function getDeltaColor(pct: number | null, inverse = false) {
    if (pct === null || pct === 0) return "text-textMuted";
    if (inverse) {
      return pct > 0 ? "text-rose-400" : "text-emerald-400";
    }
    return pct > 0 ? "text-emerald-400" : "text-rose-400";
  }
</script>

<!-- A percentage against a prior period of zero is not a percentage. `null`
     means there was nothing to compare with: "new" when this period has
     activity, a dash when neither does. -->
{#snippet pctDelta(pct: number | null, current: number, noPrior: string, inverse = false)}
  <span class="flex items-center text-[10px] font-semibold {getDeltaColor(pct, inverse)}">
    {#if pct === null}
      {#if current > 0}
        <span title={noPrior}>new</span>
      {:else}
        <span title={noPrior}>—</span>
      {/if}
    {:else if pct > 0}
      <ArrowUpRight size={11} />+{pct}%
    {:else if pct < 0}
      <ArrowDownRight size={11} />{pct}%
    {:else}
      <Minus size={11} />0%
    {/if}
  </span>
{/snippet}

<div class="gp-card p-3 rounded-xl border border-border/70 bg-surface/40 flex items-center justify-between gap-2 overflow-x-auto text-xs">
  <div class="flex items-center gap-1.5 text-textMuted font-medium text-[11px] shrink-0">
    <TrendingUp size={14} class="text-accent" />
    <span>30-Day Velocity vs Prior 30d:</span>
  </div>

  <div class="flex items-center gap-4 text-xs shrink-0">
    <!-- Commits Delta -->
    <div class="flex items-center gap-1">
      <span class="text-textMuted">Commits:</span>
      <span class="font-bold text-textPrimary">{deltas.currentCommits}</span>
      {@render pctDelta(deltas.commitsDeltaPct, deltas.currentCommits, "No commits in the prior 30 days")}
    </div>

    <!-- Additions Delta -->
    <div class="flex items-center gap-1">
      <span class="text-textMuted">Adds:</span>
      <span class="font-bold text-textPrimary">+{deltas.currentAdds.toLocaleString()}</span>
      {@render pctDelta(deltas.addsDeltaPct, deltas.currentAdds, "No lines added in the prior 30 days")}
    </div>

    <!-- Deletions Delta -->
    <div class="flex items-center gap-1">
      <span class="text-textMuted">Dels:</span>
      <span class="font-bold text-textPrimary">-{deltas.currentDels.toLocaleString()}</span>
      {@render pctDelta(deltas.delsDeltaPct, deltas.currentDels, "No lines deleted in the prior 30 days", true)}
    </div>

    <!-- Active Days Delta -->
    <div class="flex items-center gap-1">
      <span class="text-textMuted">Active Days:</span>
      <span class="font-bold text-textPrimary">{deltas.currentActiveDays}</span>
      <span class="flex items-center text-[10px] font-semibold {getDeltaColor(deltas.activeDaysDelta)}">
        {#if deltas.activeDaysDelta > 0}
          <ArrowUpRight size={11} />+{deltas.activeDaysDelta}d
        {:else if deltas.activeDaysDelta < 0}
          <ArrowDownRight size={11} />{deltas.activeDaysDelta}d
        {:else}
          <Minus size={11} />0d
        {/if}
      </span>
    </div>
  </div>
</div>
{#if !deltas.priorWindowCovered}
  <p class="text-[11px] text-textMuted -mt-1">Prior period may be outside the scanned window</p>
{/if}
