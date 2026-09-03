<script lang="ts">
  import { computePeriodCompare } from "../../pulse/metrics";
  import type { PulseCommitSummary } from "../../pulse/types";
  import { ArrowUpRight, ArrowDownRight, Minus, TrendingUp } from "lucide-svelte";

  let {
    commits = [],
  }: {
    commits: readonly PulseCommitSummary[];
  } = $props();

  const deltas = $derived(computePeriodCompare(commits));

  function getDeltaColor(pct: number, inverse = false) {
    if (pct === 0) return "text-textMuted";
    if (inverse) {
      return pct > 0 ? "text-rose-400" : "text-emerald-400";
    }
    return pct > 0 ? "text-emerald-400" : "text-rose-400";
  }
</script>

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
      <span class="flex items-center text-[10px] font-semibold {getDeltaColor(deltas.commitsDeltaPct)}">
        {#if deltas.commitsDeltaPct > 0}
          <ArrowUpRight size={11} />+{deltas.commitsDeltaPct}%
        {:else if deltas.commitsDeltaPct < 0}
          <ArrowDownRight size={11} />{deltas.commitsDeltaPct}%
        {:else}
          <Minus size={11} />0%
        {/if}
      </span>
    </div>

    <!-- Additions Delta -->
    <div class="flex items-center gap-1">
      <span class="text-textMuted">Adds:</span>
      <span class="font-bold text-textPrimary">+{deltas.currentAdds.toLocaleString()}</span>
      <span class="flex items-center text-[10px] font-semibold {getDeltaColor(deltas.addsDeltaPct)}">
        {#if deltas.addsDeltaPct > 0}
          <ArrowUpRight size={11} />+{deltas.addsDeltaPct}%
        {:else if deltas.addsDeltaPct < 0}
          <ArrowDownRight size={11} />{deltas.addsDeltaPct}%
        {:else}
          <Minus size={11} />0%
        {/if}
      </span>
    </div>

    <!-- Deletions Delta -->
    <div class="flex items-center gap-1">
      <span class="text-textMuted">Dels:</span>
      <span class="font-bold text-textPrimary">-{deltas.currentDels.toLocaleString()}</span>
      <span class="flex items-center text-[10px] font-semibold {getDeltaColor(deltas.delsDeltaPct, true)}">
        {#if deltas.delsDeltaPct > 0}
          <ArrowUpRight size={11} />+{deltas.delsDeltaPct}%
        {:else if deltas.delsDeltaPct < 0}
          <ArrowDownRight size={11} />{deltas.delsDeltaPct}%
        {:else}
          <Minus size={11} />0%
        {/if}
      </span>
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
