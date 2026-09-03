<script lang="ts">
  import type { DoraReport } from "../../pulse/types";
  import { Rocket, RefreshCw, Info } from "lucide-svelte";

  let {
    dora = null,
    error = null,
    loading = false,
    onRefresh,
  }: {
    dora: DoraReport | null;
    /** Why the tag/ancestry walk produced nothing. Never rendered as zeroes. */
    error?: string | null;
    loading?: boolean;
    onRefresh?: () => void;
  } = $props();

  function getRatingBadge(rating: string) {
    switch (rating) {
      case "Elite":
        return "bg-emerald-500/10 text-emerald-400 border-emerald-500/20";
      case "High":
        return "bg-sky-500/10 text-sky-400 border-sky-500/20";
      case "Medium":
        return "bg-amber-500/10 text-amber-400 border-amber-500/20";
      default:
        return "bg-rose-500/10 text-rose-400 border-rose-500/20";
    }
  }

  function formatHours(hours: number): string {
    if (hours <= 0) return "0h";
    if (hours < 24) return `${hours}h`;
    const days = (hours / 24).toFixed(1);
    return `${days}d`;
  }
</script>

<div class="gp-card p-4 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col gap-4">
  <!-- Header -->
  <div class="flex items-center justify-between border-b border-border/40 pb-3">
    <div>
      <div class="flex items-center gap-2">
        <Rocket size={18} class="text-sky-400" />
        <h3 class="text-sm font-semibold text-textPrimary">Delivery & Operations (DORA)</h3>
        <span class="text-xs px-2 py-0.5 rounded-full bg-surfaceMuted text-textMuted border border-border/60">
          Local-First
        </span>
      </div>
      <p class="text-xs text-textMuted mt-0.5">
        Deployment frequency and change lead times computed directly from git release tags and commit ancestry.
      </p>
    </div>

    {#if onRefresh}
      <button
        type="button"
        class="px-2.5 py-1 text-xs rounded-md bg-surface border border-border/70 text-textMuted hover:text-textPrimary flex items-center gap-1.5 transition-colors"
        disabled={loading}
        onclick={onRefresh}
      >
        <RefreshCw size={12} class={loading ? "animate-spin text-accent" : ""} />
        <span>{loading ? "Calculating..." : "Refresh DORA"}</span>
      </button>
    {/if}
  </div>

  {#if loading && !dora}
    <div class="py-12 text-center text-textMuted text-xs flex flex-col items-center gap-2">
      <RefreshCw size={20} class="animate-spin text-accent" />
      <span>Analyzing release tags and commit ancestry...</span>
    </div>
  {:else if error && !dora}
    <div class="py-8 px-4 text-center text-xs flex flex-col items-center gap-2">
      <span class="text-rose-400 font-medium">Delivery scan failed — this is not a delivery frequency of zero.</span>
      <span class="font-mono text-[11px] text-textMuted break-words max-w-lg">{error}</span>
    </div>
  {:else if !dora}
    <div class="py-8 text-center text-textMuted text-xs">
      No delivery or release data available.
    </div>
  {:else}
    <!-- 4 DORA Cards Grid -->
    <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-3">
      <!-- 1. Deployment Frequency -->
      <div class="p-3.5 rounded-xl border border-border/60 bg-surface flex flex-col justify-between">
        <div class="flex items-center justify-between text-textMuted text-xs">
          <span class="font-medium uppercase tracking-wider text-[10px]">Deploy Frequency</span>
          <span class="px-1.5 py-0.5 rounded text-[10px] font-bold border {getRatingBadge(dora.deploy_rating)}">
            {dora.deploy_rating}
          </span>
        </div>
        <div class="flex items-baseline gap-1.5 mt-2">
          <span class="text-3xl font-extrabold text-textPrimary">{dora.deploy_frequency_per_week}</span>
          <span class="text-xs text-textMuted">/ week</span>
        </div>
        <p class="text-[11px] text-textMuted mt-2">
          {dora.total_releases} release tags in last {dora.window_days}d
        </p>
      </div>

      <!-- 2. Lead Time for Changes -->
      <div class="p-3.5 rounded-xl border border-border/60 bg-surface flex flex-col justify-between">
        <div class="flex items-center justify-between text-textMuted text-xs">
          <span class="font-medium uppercase tracking-wider text-[10px]">Lead Time</span>
          <span class="px-1.5 py-0.5 rounded text-[10px] font-bold border {getRatingBadge(dora.lead_time_rating)}">
            {dora.lead_time_rating}
          </span>
        </div>
        <div class="flex items-baseline gap-1.5 mt-2">
          <span class="text-3xl font-extrabold text-textPrimary">{formatHours(dora.median_lead_time_hours)}</span>
        </div>
        <p class="text-[11px] text-textMuted mt-2">
          Commit authored → release tag
        </p>
      </div>

      <!-- 3. Change Failure Rate -->
      <div class="p-3.5 rounded-xl border border-border/60 bg-surface flex flex-col justify-between">
        <div class="flex items-center justify-between text-textMuted text-xs">
          <span class="font-medium uppercase tracking-wider text-[10px]">Change Failure Rate</span>
          <span class="px-1.5 py-0.5 rounded text-[9px] font-semibold bg-surfaceMuted text-textMuted border border-border/50">
            Heuristic
          </span>
        </div>
        <div class="flex items-baseline gap-1.5 mt-2">
          <span class="text-3xl font-extrabold text-textPrimary">{dora.change_failure_rate_pct}%</span>
        </div>
        <p class="text-[11px] text-textMuted mt-2">
          Approx. from revert & hotfix commits
        </p>
      </div>

      <!-- 4. Time to Restore Service -->
      <div class="p-3.5 rounded-xl border border-border/60 bg-surface flex flex-col justify-between">
        <div class="flex items-center justify-between text-textMuted text-xs">
          <span class="font-medium uppercase tracking-wider text-[10px]">Time to Restore</span>
          <span class="px-1.5 py-0.5 rounded text-[9px] font-semibold bg-surfaceMuted text-textMuted border border-border/50">
            Estimated
          </span>
        </div>
        <div class="flex items-baseline gap-1.5 mt-2">
          <span class="text-3xl font-extrabold text-textPrimary">
            {dora.is_mttr_approximation && dora.mttr_hours <= 0 ? "—" : formatHours(dora.mttr_hours)}
          </span>
        </div>
        <p class="text-[11px] text-textMuted mt-2">
          {dora.is_mttr_approximation && dora.mttr_hours <= 0 ? "Could not estimate from commit patterns" : "Time to follow-up patch (heuristic)"}
        </p>
      </div>
    </div>

    <!-- Honesty Invariant Explanation Banner -->
    <div class="p-2.5 rounded-lg bg-surfaceMuted/40 border border-border/40 text-[11px] text-textMuted flex items-start gap-2">
      <Info size={14} class="text-accent shrink-0 mt-0.5" />
      <span>
        <strong>Local-First Measurement:</strong> Deployment frequency and lead times are directly calculated using <code class="font-mono text-textPrimary">git tag</code> and <code class="font-mono text-textPrimary">git describe --contains</code>. Change failure rate and restore times are honest heuristic approximations derived from commit patterns.
      </span>
    </div>
  {/if}
</div>
