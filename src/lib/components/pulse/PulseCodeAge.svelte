<script lang="ts">
  import type { KnowledgeReport } from "../../pulse/types";
  import PulseStaleNotice from "./PulseStaleNotice.svelte";
  import Skeleton from "../Skeleton.svelte";
  import { describeFailure } from "../../ui/copy/describeFailure";
  import { Hourglass, Sparkles, TriangleAlert } from "@lucide/svelte";

  let {
    knowledge = null,
    error = null,
    loading = false,
    measuredAt = null,
    staleNotice = true,
    onRefresh,
  }: {
    knowledge: KnowledgeReport | null;
    /** Why the blame scan produced nothing. Never rendered as "no data". */
    error?: string | null;
    loading?: boolean;
    /** When `knowledge` was measured (epoch ms); dates the figures if a refresh fails. */
    measuredAt?: number | null;
    /**
     * Show the stale-figures notice. The knowledge tab renders this card under
     * PulseKnowledgeMap, which reports the same failed blame refresh, so it
     * turns this copy off rather than announcing the failure twice.
     */
    staleNotice?: boolean;
    /** Re-runs the blame scan. Offered on the notices for a scan that measured nothing. */
    onRefresh?: () => void;
  } = $props();

  const dist = $derived(knowledge?.age_distribution);
  const total = $derived(dist?.total_lines ?? 0);

  const pct = (val: number) => {
    if (total === 0) return 0;
    return Math.round((val / total) * 100);
  };

  /**
   * The five cohorts, oldest last. One list feeds the stacked bar and the
   * legend so the two cannot drift. The swatch is a role token (maturing takes
   * the accent: the roles have no fifth hue), and every cohort also carries
   * its range as text, so colour is never the only carrier.
   */
  const cohorts = $derived([
    { key: "fresh", name: "Fresh (< 30d)", range: "< 30 days", lines: dist?.fresh_lines ?? 0, swatch: "bg-ok" },
    { key: "recent", name: "Recent (30-90d)", range: "30–90 days", lines: dist?.recent_lines ?? 0, swatch: "bg-info" },
    { key: "maturing", name: "Maturing (90-365d)", range: "90–365 days", lines: dist?.maturing_lines ?? 0, swatch: "bg-accent" },
    { key: "legacy", name: "Legacy (1-2y)", range: "1–2 years", lines: dist?.legacy_lines ?? 0, swatch: "bg-warn" },
    { key: "ancient", name: "Ancient (> 2y)", range: "> 2 years", lines: dist?.ancient_lines ?? 0, swatch: "bg-danger" },
  ].map((c) => ({ ...c, pct: pct(c.lines) })));

  const ninetyDayTotalPct = $derived(
    total > 0
      ? Math.round((((dist?.fresh_lines ?? 0) + (dist?.recent_lines ?? 0)) / total) * 100)
      : 0
  );

  const failure = $derived(error ? describeFailure({ verb: "read", object: "blame data" }, error) : null);
</script>

<div class="gp-card p-4 flex flex-col gap-4">
  <!-- Header -->
  <div class="flex items-center justify-between gap-3">
    <div>
      <div class="flex items-center gap-2">
        <Hourglass size={18} class="text-info" />
        <h3 class="text-sm font-semibold text-textPrimary">Code age and half-life</h3>
      </div>
      <p class="text-xs text-textMuted mt-0.5">
        Generational distribution of surviving lines based on blame timestamps.
      </p>
    </div>

    {#if knowledge}
      <span class="gp-chip shrink-0" data-tone="info" data-kind="label">
        <Sparkles size={12} />
        <span>{ninetyDayTotalPct}% written in last 90 days</span>
      </span>
    {/if}
  </div>

  {#if loading && !knowledge}
    <div class="flex flex-col gap-2">
      <Skeleton variant="rect" height="4rem" />
      <Skeleton variant="rect" height="0.75rem" />
      <p class="m-0 text-xs text-textMuted">Calculating code age distribution…</p>
    </div>
  {:else if error && !knowledge}
    <div class="gp-notice" data-tone="unknown" role="status">
      <TriangleAlert size={14} class="mt-px shrink-0" style="color: var(--gp-notice-ink)" />
      <div class="min-w-0 flex-1">
        <p class="m-0 font-medium">{failure?.headline} No age distribution was measured.</p>
        {#if failure?.detail}
          <p class="m-0 mt-0.5 font-mono text-meta text-textMuted wrap-break-word">{failure.detail}</p>
        {/if}
      </div>
      {#if onRefresh}
        <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={() => onRefresh?.()}>Rescan</button>
      {/if}
    </div>
  {:else if !knowledge || total === 0}
    <div class="gp-notice" data-tone="unknown" role="status">
      <TriangleAlert size={14} class="mt-px shrink-0" style="color: var(--gp-notice-ink)" />
      <div class="min-w-0 flex-1">
        <p class="m-0 font-medium">Couldn't measure code age.</p>
        <p class="m-0 mt-0.5 text-textMuted">The blame scan did not date any lines.</p>
      </div>
      {#if onRefresh}
        <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={() => onRefresh?.()}>Rescan</button>
      {/if}
    </div>
  {:else}
    {#if error && staleNotice}
      <PulseStaleNotice {error} {measuredAt} />
    {/if}
    <!-- Headline half-life card -->
    <div class="flex flex-col sm:flex-row sm:items-center justify-between gap-4 p-3 rounded-lg bg-surfaceHover/40">
      <div>
        <span class="gp-label">Repository code half-life</span>
        <div class="flex items-baseline gap-2 mt-1">
          <span class="text-display font-semibold text-textPrimary">{knowledge.half_life_days}</span>
          <span class="text-sm text-textMuted font-medium">days</span>
        </div>
        <p class="text-meta text-textMuted mt-0.5">
          50% of the live codebase was written or touched in the last {knowledge.half_life_days} days.
        </p>
      </div>

      <div class="text-xs text-textMuted text-right">
        <div class="font-medium text-textPrimary">{total.toLocaleString()} live lines analyzed</div>
        <div class="text-meta text-textMuted mt-0.5">Across {knowledge.scanned_files} files</div>
      </div>
    </div>

    <!-- Generational stacked bar -->
    <div class="flex flex-col gap-2">
      <span class="gp-label">Age cohorts</span>
      <div class="w-full h-3 rounded-full overflow-hidden flex bg-surfaceHover">
        {#each cohorts as cohort (cohort.key)}
          {#if cohort.pct > 0}
            <div style="width: {cohort.pct}%;" class={cohort.swatch} title="{cohort.name}: {cohort.pct}%"></div>
          {/if}
        {/each}
      </div>

      <!-- Cohorts legend grid -->
      <div class="grid grid-cols-2 sm:grid-cols-5 gap-2 mt-2">
        {#each cohorts as cohort (cohort.key)}
          <div class="p-2 rounded bg-surfaceHover/40">
            <div class="flex items-center gap-1.5 text-textPrimary text-xs font-semibold">
              <span class="w-2 h-2 rounded-full {cohort.swatch}"></span>
              <span>{cohort.range}</span>
            </div>
            <div class="text-sm font-semibold text-textPrimary mt-1">{cohort.pct}%</div>
            <div class="text-micro text-textMuted truncate">{cohort.lines.toLocaleString()} lines</div>
          </div>
        {/each}
      </div>
    </div>
  {/if}
</div>
