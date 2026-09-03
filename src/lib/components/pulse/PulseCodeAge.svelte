<script lang="ts">
  import type { KnowledgeReport } from "../../pulse/types";
  import { Hourglass, Sparkles } from "lucide-svelte";

  let {
    knowledge = null,
    loading = false,
  }: {
    knowledge: KnowledgeReport | null;
    loading?: boolean;
  } = $props();

  const dist = $derived(knowledge?.age_distribution);
  const total = $derived(dist?.total_lines ?? 0);

  const pct = (val: number) => {
    if (total === 0) return 0;
    return Math.round((val / total) * 100);
  };

  const freshPct = $derived(pct(dist?.fresh_lines ?? 0));
  const recentPct = $derived(pct(dist?.recent_lines ?? 0));
  const maturingPct = $derived(pct(dist?.maturing_lines ?? 0));
  const legacyPct = $derived(pct(dist?.legacy_lines ?? 0));
  const ancientPct = $derived(pct(dist?.ancient_lines ?? 0));

  const ninetyDayTotalPct = $derived(
    total > 0
      ? Math.round((((dist?.fresh_lines ?? 0) + (dist?.recent_lines ?? 0)) / total) * 100)
      : 0
  );
</script>

<div class="gp-card p-4 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col gap-4">
  <!-- Header -->
  <div class="flex items-center justify-between border-b border-border/40 pb-3">
    <div>
      <div class="flex items-center gap-2">
        <Hourglass size={18} class="text-cyan-400" />
        <h3 class="text-sm font-semibold text-textPrimary">Code Age & Half-Life</h3>
      </div>
      <p class="text-xs text-textMuted mt-0.5">
        Generational distribution of surviving lines based on blame timestamps.
      </p>
    </div>

    {#if knowledge}
      <div class="flex items-center gap-1.5 px-2.5 py-1 rounded-full bg-cyan-500/10 border border-cyan-500/20 text-cyan-400 text-xs font-semibold">
        <Sparkles size={12} />
        <span>{ninetyDayTotalPct}% written in last 90 days</span>
      </div>
    {/if}
  </div>

  {#if loading && !knowledge}
    <div class="py-8 text-center text-textMuted text-xs">
      Calculating code age distribution...
    </div>
  {:else if !knowledge || total === 0}
    <div class="py-8 text-center text-textMuted text-xs">
      No code age data available.
    </div>
  {:else}
    <!-- Headline Half-Life Card -->
    <div class="flex flex-col sm:flex-row sm:items-center justify-between gap-4 p-3 rounded-lg border border-border/60 bg-surface">
      <div>
        <span class="text-xs uppercase font-medium tracking-wider text-textMuted">Repo Code Half-Life</span>
        <div class="flex items-baseline gap-2 mt-1">
          <span class="text-3xl font-extrabold text-textPrimary">{knowledge.half_life_days}</span>
          <span class="text-sm text-textMuted font-medium">days</span>
        </div>
        <p class="text-[11px] text-textMuted mt-0.5">
          50% of the live codebase was written or touched in the last {knowledge.half_life_days} days.
        </p>
      </div>

      <div class="text-xs text-textMuted text-right sm:border-l sm:border-border/50 sm:pl-4">
        <div class="font-medium text-textPrimary">{total.toLocaleString()} live lines analyzed</div>
        <div class="text-[11px] text-textMuted mt-0.5">Across {knowledge.scanned_files} files</div>
      </div>
    </div>

    <!-- Generational Stacked Bar -->
    <div class="flex flex-col gap-2">
      <span class="text-xs font-semibold uppercase tracking-wider text-textMuted">Generational Age Cohorts</span>
      <div class="w-full h-3 rounded-full overflow-hidden flex bg-surfaceMuted">
        {#if freshPct > 0}
          <div style="width: {freshPct}%;" class="bg-emerald-400 transition-all duration-500" title="Fresh (< 30d): {freshPct}%"></div>
        {/if}
        {#if recentPct > 0}
          <div style="width: {recentPct}%;" class="bg-cyan-400 transition-all duration-500" title="Recent (30-90d): {recentPct}%"></div>
        {/if}
        {#if maturingPct > 0}
          <div style="width: {maturingPct}%;" class="bg-indigo-400 transition-all duration-500" title="Maturing (90-365d): {maturingPct}%"></div>
        {/if}
        {#if legacyPct > 0}
          <div style="width: {legacyPct}%;" class="bg-amber-400 transition-all duration-500" title="Legacy (1-2y): {legacyPct}%"></div>
        {/if}
        {#if ancientPct > 0}
          <div style="width: {ancientPct}%;" class="bg-rose-400 transition-all duration-500" title="Ancient (> 2y): {ancientPct}%"></div>
        {/if}
      </div>

      <!-- Cohorts Legend Grid -->
      <div class="grid grid-cols-2 sm:grid-cols-5 gap-2 mt-2">
        <div class="p-2 rounded bg-surface/40 border border-border/40">
          <div class="flex items-center gap-1.5 text-emerald-400 text-xs font-semibold">
            <span class="w-2 h-2 rounded-full bg-emerald-400"></span>
            <span>&lt; 30 days</span>
          </div>
          <div class="text-sm font-bold text-textPrimary mt-1">{freshPct}%</div>
          <div class="text-[10px] text-textMuted truncate">{(dist?.fresh_lines ?? 0).toLocaleString()} lines</div>
        </div>

        <div class="p-2 rounded bg-surface/40 border border-border/40">
          <div class="flex items-center gap-1.5 text-cyan-400 text-xs font-semibold">
            <span class="w-2 h-2 rounded-full bg-cyan-400"></span>
            <span>30–90 days</span>
          </div>
          <div class="text-sm font-bold text-textPrimary mt-1">{recentPct}%</div>
          <div class="text-[10px] text-textMuted truncate">{(dist?.recent_lines ?? 0).toLocaleString()} lines</div>
        </div>

        <div class="p-2 rounded bg-surface/40 border border-border/40">
          <div class="flex items-center gap-1.5 text-indigo-400 text-xs font-semibold">
            <span class="w-2 h-2 rounded-full bg-indigo-400"></span>
            <span>90–365 days</span>
          </div>
          <div class="text-sm font-bold text-textPrimary mt-1">{maturingPct}%</div>
          <div class="text-[10px] text-textMuted truncate">{(dist?.maturing_lines ?? 0).toLocaleString()} lines</div>
        </div>

        <div class="p-2 rounded bg-surface/40 border border-border/40">
          <div class="flex items-center gap-1.5 text-amber-400 text-xs font-semibold">
            <span class="w-2 h-2 rounded-full bg-amber-400"></span>
            <span>1–2 years</span>
          </div>
          <div class="text-sm font-bold text-textPrimary mt-1">{legacyPct}%</div>
          <div class="text-[10px] text-textMuted truncate">{(dist?.legacy_lines ?? 0).toLocaleString()} lines</div>
        </div>

        <div class="p-2 rounded bg-surface/40 border border-border/40">
          <div class="flex items-center gap-1.5 text-rose-400 text-xs font-semibold">
            <span class="w-2 h-2 rounded-full bg-rose-400"></span>
            <span>&gt; 2 years</span>
          </div>
          <div class="text-sm font-bold text-textPrimary mt-1">{ancientPct}%</div>
          <div class="text-[10px] text-textMuted truncate">{(dist?.ancient_lines ?? 0).toLocaleString()} lines</div>
        </div>
      </div>
    </div>
  {/if}
</div>
