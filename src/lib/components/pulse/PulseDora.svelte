<script lang="ts">
  import type { DoraReport } from "../../pulse/types";
  import PulseStaleNotice from "./PulseStaleNotice.svelte";
  import EmptyState from "../EmptyState.svelte";
  import Skeleton from "../Skeleton.svelte";
  import { describeFailure } from "../../ui/copy/describeFailure";
  import { Rocket, RefreshCw, Info, TriangleAlert } from "@lucide/svelte";

  let {
    dora = null,
    error = null,
    loading = false,
    measuredAt = null,
    onRefresh,
  }: {
    dora: DoraReport | null;
    /** Why the tag/ancestry walk produced nothing. Never rendered as zeroes. */
    error?: string | null;
    loading?: boolean;
    /** When `dora` was measured (epoch ms); dates the figures if a refresh fails. */
    measuredAt?: number | null;
    onRefresh?: () => void;
  } = $props();

  /**
   * The tone a rating wears. An unrecognised rating is "unknown", not the
   * worst tier: a label the view cannot read must not pass for a verdict.
   */
  function ratingTone(rating: string): "ok" | "info" | "warn" | "danger" | "unknown" {
    switch (rating) {
      case "Elite":
        return "ok";
      case "High":
        return "info";
      case "Medium":
        return "warn";
      case "Low":
        return "danger";
      default:
        return "unknown";
    }
  }

  function formatHours(hours: number): string {
    if (hours <= 0) return "0h";
    if (hours < 24) return `${hours}h`;
    const days = (hours / 24).toFixed(1);
    return `${days}d`;
  }

  const failure = $derived(error ? describeFailure({ verb: "scan", object: "release tags" }, error) : null);

  /**
   * One sentence, appended to both commit-derived cards.
   *
   * The change-failure rate and the restore time come from a single capped
   * `git log`, so a cut scan bounds both and the caveat belongs on each tile
   * rather than once for the section — the same rule the export card follows.
   * Card 1 states releases over the whole window beside them, so without this
   * a reader has every reason to read the other two as covering it too.
   */
  const scanCaveat = $derived(
    !dora?.commit_scan_truncated
      ? ""
      : dora.commit_scan_window_commits
        ? ` — the newest of ${dora.commit_scan_window_commits} in the window, not all of it`
        : " — the newest in the window, not all of it",
  );
</script>

<div class="gp-card p-4 flex flex-col gap-4">
  <!-- Header -->
  <div class="flex items-center justify-between gap-3">
    <div>
      <div class="flex items-center gap-2">
        <Rocket size={18} class="text-info" />
        <h3 class="text-sm font-semibold text-textPrimary">Delivery and operations (DORA)</h3>
        <span class="gp-chip" data-tone="neutral">Local-first</span>
      </div>
      <p class="text-xs text-textMuted mt-0.5">
        Deployment frequency and change lead times computed directly from git release tags and commit ancestry.
      </p>
    </div>

    {#if onRefresh}
      <button type="button" class="gp-btn shrink-0" data-size="sm" disabled={loading} onclick={onRefresh}>
        <RefreshCw size={12} class={loading ? "animate-spin text-accent" : ""} />
        <span>{loading ? "Refreshing…" : "Refresh"}</span>
      </button>
    {/if}
  </div>

  {#if loading && !dora}
    <div class="flex flex-col gap-3">
      <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-3">
        {#each [0, 1, 2, 3] as slot (slot)}
          <Skeleton variant="rect" height="6rem" />
        {/each}
      </div>
      <p class="m-0 text-xs text-textMuted">Analyzing release tags and commit ancestry…</p>
    </div>
  {:else if error && !dora}
    <div class="gp-notice" data-tone="unknown" role="status">
      <TriangleAlert size={14} class="mt-px shrink-0" style="color: var(--gp-notice-ink)" />
      <div class="min-w-0 flex-1">
        <p class="m-0 font-medium">{failure?.headline} This is not a delivery frequency of zero.</p>
        {#if failure?.detail}
          <p class="m-0 mt-0.5 font-mono text-meta text-textMuted wrap-break-word">{failure.detail}</p>
        {/if}
      </div>
      {#if onRefresh}
        <button type="button" class="gp-btn shrink-0" data-size="sm" disabled={loading} onclick={onRefresh}>
          {failure?.retryLabel ?? "Try again"}
        </button>
      {/if}
    </div>
  {:else if !dora}
    <EmptyState
      compact
      icon={Rocket}
      title="No delivery report yet"
      hint="Refresh to measure deployment frequency and lead time from release tags."
      action={onRefresh ? { label: "Refresh", onClick: onRefresh, variant: "secondary" } : undefined}
    />
  {:else}
    {#if error}
      <PulseStaleNotice {error} {measuredAt} />
    {/if}
    <!-- The four DORA tiles -->
    <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-3">
      <!-- 1. Deployment frequency -->
      <div class="p-3.5 rounded-xl bg-surfaceHover/40 flex flex-col justify-between">
        <div class="flex items-center justify-between gap-2 text-textMuted text-xs">
          <span class="gp-label">Deploy frequency</span>
          <span class="gp-chip" data-tone={ratingTone(dora.deploy_rating)}>{dora.deploy_rating}</span>
        </div>
        <div class="flex items-baseline gap-1.5 mt-2">
          <span class="text-display font-semibold text-textPrimary">{dora.deploy_frequency_per_week}</span>
          <span class="text-xs text-textMuted">/ week</span>
        </div>
        <p class="text-meta text-textMuted mt-2">
          {dora.total_releases} release tags in last {dora.window_days}d
        </p>
      </div>

      <!-- 2. Lead time for changes -->
      <div class="p-3.5 rounded-xl bg-surfaceHover/40 flex flex-col justify-between">
        <div class="flex items-center justify-between gap-2 text-textMuted text-xs">
          <span class="gp-label">Lead time</span>
          <span class="gp-chip" data-tone={ratingTone(dora.lead_time_rating)}>{dora.lead_time_rating}</span>
        </div>
        <div class="flex items-baseline gap-1.5 mt-2">
          <span class="text-display font-semibold text-textPrimary">{formatHours(dora.median_lead_time_hours)}</span>
        </div>
        <p class="text-meta text-textMuted mt-2">
          Commit authored → release tag
        </p>
      </div>

      <!-- 3. Change failure rate -->
      <div class="p-3.5 rounded-xl bg-surfaceHover/40 flex flex-col justify-between">
        <div class="flex items-center justify-between gap-2 text-textMuted text-xs">
          <span class="gp-label">Change failure rate</span>
          <!-- Derived from the flag the backend already sends, not hard-coded.
               `is_cfr_approximation` was declared, set by Rust and read by
               nobody, so a measured change-failure rate would have arrived and
               still been labelled a heuristic — a stale label on a number is
               worse than no label, because it is believed. Card 4 has read its
               own flag all along; this one now matches it. -->
          <span class="gp-chip" data-tone="neutral">
            {dora.is_cfr_approximation ? "Heuristic" : "Measured"}
          </span>
        </div>
        <div class="flex items-baseline gap-1.5 mt-2">
          <span class="text-display font-semibold text-textPrimary">
            {dora.cfr_sample_commits <= 0 ? "—" : `${dora.change_failure_rate_pct}%`}
          </span>
        </div>
        <p class="text-meta text-textMuted mt-2">
          <!-- Three guards, and the nesting is the decision. The empty-sample
               check is outermost so this line always agrees with the value
               above it, which shows an em dash on an empty sample whatever the
               flag says — a caption naming a source for a number that is not
               being shown would be its own small dishonesty. Inside it the
               wording follows `is_cfr_approximation` rather than assuming the
               heuristic, and the heuristic branch names the denominator it was
               measured over instead of letting the reader assume the window.
               `scanCaveat` rides only on that branch: it describes the commit
               scan, so it belongs to the rate derived from commits and not to
               one read from recorded deployment outcomes. -->
          {dora.cfr_sample_commits <= 0
            ? "No commits in this window to examine"
            : dora.is_cfr_approximation
              ? `Approximated from reverts and hotfixes in ${dora.cfr_sample_commits} examined ${dora.cfr_sample_commits === 1 ? "commit" : "commits"}${scanCaveat}`
              : "From recorded deployment outcomes"}
        </p>
      </div>

      <!-- 4. Time to restore service -->
      <div class="p-3.5 rounded-xl bg-surfaceHover/40 flex flex-col justify-between">
        <div class="flex items-center justify-between gap-2 text-textMuted text-xs">
          <span class="gp-label">Time to restore</span>
          <span class="gp-chip" data-tone="neutral">Estimated</span>
        </div>
        <div class="flex items-baseline gap-1.5 mt-2">
          <span class="text-display font-semibold text-textPrimary">
            {dora.is_mttr_approximation && dora.mttr_hours <= 0 ? "—" : formatHours(dora.mttr_hours)}
          </span>
        </div>
        <p class="text-meta text-textMuted mt-2">
          {dora.is_mttr_approximation && dora.mttr_hours <= 0 ? "Couldn't estimate from commit patterns" : `Time to follow-up patch (heuristic)${scanCaveat}`}
        </p>
      </div>
    </div>

    <!-- How the numbers are measured -->
    <div class="gp-notice" data-tone="info">
      <Info size={14} class="shrink-0 mt-0.5" style="color: var(--gp-notice-ink)" />
      <span class="min-w-0 flex-1 text-meta">
        <span class="font-semibold">Local-first measurement:</span> Deployment frequency and lead times are calculated directly from <code class="font-mono text-textPrimary">git tag</code> and <code class="font-mono text-textPrimary">git describe --contains</code>. Change failure rate and restore time are heuristic approximations derived from commit patterns.
      </span>
    </div>
  {/if}
</div>
