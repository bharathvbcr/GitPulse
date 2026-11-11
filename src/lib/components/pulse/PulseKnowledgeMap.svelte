<script lang="ts">
  import type { KnowledgeReport } from "../../pulse/types";
  import PulseStaleNotice from "./PulseStaleNotice.svelte";
  import EmptyState from "../EmptyState.svelte";
  import Skeleton from "../Skeleton.svelte";
  import { describeFailure } from "../../ui/copy/describeFailure";
  import { Users, AlertTriangle, ShieldCheck, FileWarning, RefreshCw } from "@lucide/svelte";

  let {
    knowledge = null,
    error = null,
    loading = false,
    measuredAt = null,
    onRefresh,
  }: {
    knowledge: KnowledgeReport | null;
    /** Why the blame scan produced nothing. Never rendered as "no data". */
    error?: string | null;
    loading?: boolean;
    /** When `knowledge` was measured (epoch ms); dates the figures if a refresh fails. */
    measuredAt?: number | null;
    onRefresh?: () => void;
  } = $props();

  /** Authors listed in the ownership breakdown; the rest are counted, not drawn. */
  const AUTHORS_SHOWN = 8;

  /** Areas the sample never blamed: their authors are absent from every figure. */
  const unsampledAreas = $derived(
    (knowledge?.areas ?? [])
      .filter((a) => a.sampled_files === 0)
      .map((a) => (a.area === "" ? "(top-level files)" : a.area)),
  );

  function formatRelativeDays(timestampSecs: number): string {
    if (!timestampSecs) return "Unknown";
    const nowSecs = Math.floor(Date.now() / 1000);
    const diffDays = Math.floor((nowSecs - timestampSecs) / 86400);
    if (diffDays <= 0) return "today";
    if (diffDays === 1) return "yesterday";
    if (diffDays < 30) return `${diffDays} days ago`;
    const months = Math.floor(diffDays / 30);
    return `${months} ${months === 1 ? 'month' : 'months'} ago`;
  }
</script>

<div class="gp-card p-4 flex flex-col gap-4">
  <!-- Header -->
  <div class="flex items-center justify-between gap-3">
    <div>
      <div class="flex items-center gap-2">
        <Users size={18} class="text-accent" />
        <h3 class="text-sm font-semibold text-textPrimary">Knowledge distribution and bus factor</h3>
      </div>
      <p class="text-xs text-textMuted mt-0.5">
        Line ownership and team concentration, from git blame analysis.
      </p>
    </div>

    {#if onRefresh}
      <button
        type="button"
        class="gp-btn shrink-0"
        data-variant="secondary"
        data-size="sm"
        disabled={loading}
        onclick={onRefresh}
      >
        <RefreshCw size={12} class={loading ? "animate-spin text-accent" : ""} />
        <span>{loading ? "Scanning…" : "Rescan blame"}</span>
      </button>
    {/if}
  </div>

  {#if loading && !knowledge}
    <div class="flex flex-col gap-2 py-2">
      <Skeleton variant="rect" count={3} height="4rem" />
      <p class="gp-label text-center">Scanning blame across repository files…</p>
    </div>
  {:else if error && !knowledge}
    {@const failure = describeFailure({ verb: "scan", object: "blame" }, error)}
    <div class="gp-notice" data-tone="unknown" role="status">
      <AlertTriangle size={14} class="shrink-0 mt-px" style="color: var(--gp-notice-ink)" />
      <div class="min-w-0 flex-1">
        <p class="font-medium">{failure.headline} This is not a bus factor of zero.</p>
        <p class="font-mono text-meta text-textMuted wrap-break-word mt-1">{error}</p>
      </div>
    </div>
  {:else if !knowledge || knowledge.scanned_files === 0}
    <EmptyState
      compact
      icon={Users}
      title="No blame data yet"
      hint="Rescan to read line ownership across the repository."
    />
  {:else}
    <!-- Key Bus Factor & Scan Stat Row -->
    {#if error}
      <PulseStaleNotice {error} {measuredAt} />
    {/if}
    <div class="grid grid-cols-1 sm:grid-cols-3 gap-3">
      <!-- Bus Factor Card -->
      <div class="p-3 rounded-lg bg-background flex flex-col justify-between">
        <div class="flex items-center justify-between text-textMuted text-xs">
          <span class="gp-label">Bus factor</span>
          {#if knowledge.bus_factor === 1}
            <span class="gp-chip" data-kind="label" data-tone="danger">Critical</span>
          {:else if knowledge.bus_factor === 2}
            <span class="gp-chip" data-kind="label" data-tone="warn">Moderate</span>
          {:else}
            <span class="gp-chip" data-kind="label" data-tone="ok">Healthy</span>
          {/if}
        </div>
        <div class="flex items-baseline gap-2 mt-2">
          <span class="text-3xl font-semibold {knowledge.bus_factor === 1 ? 'text-danger' : knowledge.bus_factor === 2 ? 'text-warn' : 'text-ok'}">
            {knowledge.bus_factor}
          </span>
          <span class="text-xs text-textMuted font-medium">key contributor{knowledge.bus_factor === 1 ? '' : 's'}</span>
        </div>
        <p class="text-meta text-textMuted mt-1">
          {knowledge.bus_factor === 1
            ? "1 contributor owns at least 50% of surviving code."
            : `${knowledge.bus_factor} contributors hold at least 50% of surviving code.`}
        </p>
      </div>

      <!-- Scanned Coverage -->
      <div class="p-3 rounded-lg bg-background flex flex-col justify-between">
        <div class="flex items-center justify-between text-textMuted text-xs">
          <span class="gp-label">Scanned files</span>
          <ShieldCheck size={14} class="text-accent" />
        </div>
        <div class="flex items-baseline gap-2 mt-2">
          <span class="text-3xl font-semibold text-textPrimary">{knowledge.scanned_files}</span>
          <span class="text-xs text-textMuted">of {knowledge.candidate_files} candidate files</span>
        </div>
        <p class="text-meta text-textMuted mt-1">
          {knowledge.scanned_lines.toLocaleString()} live lines analyzed in {knowledge.duration_ms}ms
        </p>
      </div>

      <!-- Orphaned Files Count -->
      <div class="p-3 rounded-lg bg-background flex flex-col justify-between">
        <div class="flex items-center justify-between text-textMuted text-xs">
          <span class="gp-label">Orphaned code</span>
          <AlertTriangle size={14} class={knowledge.orphaned_files.length > 0 ? "text-warn" : "text-ok"} />
        </div>
        <div class="flex items-baseline gap-2 mt-2">
          <span class="text-3xl font-semibold {knowledge.orphaned_files.length > 0 ? 'text-warn' : 'text-ok'}">
            {knowledge.orphaned_files.length}
          </span>
          <span class="text-xs text-textMuted">stale files</span>
        </div>
        <p class="text-meta text-textMuted mt-1">
          Primary author inactive in this repository for over 6 months
        </p>
      </div>
    </div>

    <!-- Team Authors Breakdown -->
    <div class="flex flex-col gap-2.5">
      <h4 class="gp-label">Primary code ownership</h4>
      <div class="flex flex-col gap-2">
        {#each knowledge.primary_authors.slice(0, AUTHORS_SHOWN) as author}
          <div class="flex flex-col gap-1 p-2 rounded-lg bg-background">
            <div class="flex items-center justify-between text-xs">
              <div class="flex items-center gap-2">
                <span class="font-medium text-textPrimary">{author.author_name}</span>
                <span class="text-micro text-textMuted font-mono truncate max-w-xs">{author.author_email}</span>
              </div>
              <div class="flex items-center gap-2">
                <span class="text-textMuted text-meta">{author.lines_owned.toLocaleString()} lines</span>
                <span class="font-semibold text-textPrimary text-xs">{author.percentage}%</span>
              </div>
            </div>
            <!-- Progress Bar -->
            <div class="w-full h-1.5 bg-surfaceHover rounded-full overflow-hidden">
              <div
                class="h-full bg-accent rounded-full"
                style="width: {Math.min(100, author.percentage)}%;"
              ></div>
            </div>
          </div>
        {/each}
      </div>
      {#if knowledge.primary_authors.length > AUTHORS_SHOWN}
        <p class="text-meta text-textMuted">
          Showing {AUTHORS_SHOWN} of {knowledge.primary_authors.length} authors.
        </p>
      {/if}
    </div>

    <!-- Orphaned Files Table if present -->
    {#if knowledge.orphaned_files.length > 0}
      <div class="flex flex-col gap-2 mt-2">
        <div class="flex items-center gap-1.5 text-warn text-xs font-semibold">
          <FileWarning size={14} />
          <h4>Orphaned code: owner inactive for over 180 days</h4>
        </div>
        <div class="overflow-x-auto max-h-48 overflow-y-auto rounded-lg bg-background">
          <table class="w-full text-left text-xs border-collapse">
            <thead class="bg-surfaceHover text-textMuted text-meta font-medium sticky top-0">
              <tr>
                <th class="py-1.5 px-3 font-medium">File path</th>
                <th class="py-1.5 px-3 font-medium">Primary author</th>
                <th class="py-1.5 px-3 font-medium">Live lines</th>
                <th class="py-1.5 px-3 font-medium text-right">Last owner commit</th>
              </tr>
            </thead>
            <tbody>
              {#each knowledge.orphaned_files as file}
                <tr class="hover:bg-surfaceHover/50 font-mono text-meta">
                  <td class="py-1.5 px-3 truncate max-w-xs text-textPrimary" title={file.path}>{file.path}</td>
                  <td class="py-1.5 px-3 text-textMuted font-sans">{file.primary_author}</td>
                  <td class="py-1.5 px-3 text-textMuted">{file.lines_count.toLocaleString()}</td>
                  <td class="py-1.5 px-3 text-right text-warn font-sans">
                    {formatRelativeDays(file.last_commit_timestamp)}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </div>
    {/if}

    <!-- Truncation / Honesty Note -->
    {#if knowledge.truncated}
      <div class="gp-notice" data-tone="info" role="note">
        <div class="min-w-0 flex-1 text-meta">
          Sampled {knowledge.scanned_files} of {knowledge.candidate_files} files at HEAD, highest
          churn first in each of {knowledge.areas_total} areas.
          {#if unsampledAreas.length > 0}
            Not sampled: {unsampledAreas.join(", ")}{knowledge.areas_total > knowledge.areas.length
              ? ` and ${knowledge.areas_total - knowledge.areas.length} more`
              : ""}.
          {/if}
          {#if knowledge.churn_unavailable}
            Churn could not be read, so files were taken in path order.
          {:else if knowledge.churn_window_capped}
            Churn covers only the newest commits; a file busy only before that ranks low.
          {/if}
        </div>
      </div>
    {/if}
    {#if knowledge.submodules_skipped > 0}
      <div class="text-meta text-textMuted">
        {knowledge.submodules_skipped} submodule{knowledge.submodules_skipped === 1 ? "" : "s"} not blamed.
      </div>
    {/if}
  {/if}
</div>
