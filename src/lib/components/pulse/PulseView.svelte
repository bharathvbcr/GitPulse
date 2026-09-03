<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { repoStore } from "../../stores/repoStore";
  import { pulseStore } from "../../pulse/pulseStore";
  import PulseHeatmap from "./PulseHeatmap.svelte";
  import PulseRhythm from "./PulseRhythm.svelte";
  import PulsePunchCard from "./PulsePunchCard.svelte";
  import PulseLineTrend from "./PulseLineTrend.svelte";
  import PulseHygiene from "./PulseHygiene.svelte";
  import EmptyState from "../EmptyState.svelte";
  import type { LanguageStatsReport } from "../../language/barStats";
  import {
    Activity,
    AlertCircle,
    FileText,
    GitCommit,
    Mail,
    RefreshCw,
  } from "lucide-svelte";

  let totalLoc = $state(0);

  // Fetch report on repo switch or mount
  $effect(() => {
    const path = $repoStore.currentPath;
    if (path) {
      void pulseStore.load(path);
      void fetchTotalLoc(path);
    }
  });

  async function fetchTotalLoc(path: string) {
    try {
      const langReport = await invoke<LanguageStatsReport>("cmd_get_language_stats", {
        repoPath: path,
      });
      if (langReport?.stats) {
        totalLoc = langReport.stats.reduce(
          (sum: number, s) => sum + (s.code_lines ?? 0),
          0,
        );
      }
    } catch {
      // Non-fatal: historical trend will anchor without total
      totalLoc = 0;
    }
  }

  function handleDeepenScan() {
    if ($repoStore.currentPath) {
      void pulseStore.setLimit(25_000);
    }
  }

  const report = $derived($pulseStore.report);
  const loading = $derived($pulseStore.loading);
  const error = $derived($pulseStore.error);
</script>

<div class="flex-1 flex flex-col min-h-0 bg-background overflow-y-auto">
  <!-- View Header Bar -->
  <div class="px-6 py-4 border-b border-border/80 bg-surface/40 shrink-0 flex flex-col sm:flex-row sm:items-center justify-between gap-3">
    <div class="flex items-center gap-3">
      <div class="w-9 h-9 rounded-xl bg-accent/10 border border-accent/25 flex items-center justify-center text-accent shadow-sm">
        <Activity size={18} />
      </div>
      <div>
        <div class="flex items-center gap-2">
          <h1 class="text-sm font-bold text-textPrimary tracking-tight">Repository Pulse</h1>
          {#if report}
            <span class="text-[10px] font-mono px-2 py-0.5 rounded-full bg-surface border border-border text-textMuted">
              {report.total_commits_scanned.toLocaleString()} commits
            </span>
          {/if}
          {#if report?.has_mailmap}
            <span
              class="text-[10px] px-2 py-0.5 rounded-full bg-emerald-500/10 text-emerald-500 border border-emerald-500/25 flex items-center gap-1"
              title=".mailmap active: author aliases are canonicalized."
            >
              <Mail size={10} />
              <span>.mailmap</span>
            </span>
          {/if}
        </div>
        <p class="text-[11px] text-textMuted mt-0.5">
          Engineering rhythm, contribution calendar, commit hygiene, and line churn.
        </p>
      </div>
    </div>

    <!-- Actions & Status Badges -->
    <div class="flex items-center gap-2">
      {#if report?.duration_ms}
        <span class="text-[10px] text-textMuted font-mono hidden md:inline">
          {report.duration_ms}ms
        </span>
      {/if}
      <button
        type="button"
        onclick={() => pulseStore.reload()}
        disabled={loading}
        class="gp-btn !py-1.5 !px-3 text-xs inline-flex items-center gap-1.5"
      >
        <RefreshCw size={12} class={loading ? 'animate-spin text-accent' : ''} />
        <span>{loading ? 'Refreshing…' : 'Refresh'}</span>
      </button>
    </div>
  </div>

  <!-- Truncation Warning / Notice -->
  {#if report?.truncated}
    <div class="mx-6 mt-4 p-3 rounded-xl bg-amber-500/10 border border-amber-500/30 text-xs text-textPrimary flex items-center justify-between gap-3">
      <div class="flex items-center gap-2 text-amber-500">
        <AlertCircle size={15} class="shrink-0" />
        <span class="font-medium">History bounded at {report.total_commits_scanned.toLocaleString()} commits.</span>
        <span class="text-textMuted hidden sm:inline">Older commits exist in this repository.</span>
      </div>
      {#if $pulseStore.maxCommits < 25_000}
        <button
          type="button"
          onclick={handleDeepenScan}
          disabled={loading}
          class="gp-btn !py-1 !px-2.5 !text-[11px] shrink-0 font-medium"
        >
          Scan Deeper (25k)
        </button>
      {/if}
    </div>
  {/if}

  <!-- Error State -->
  {#if error}
    <div class="m-6 p-4 rounded-xl bg-rose-500/10 border border-rose-500/30 text-xs text-rose-500 flex items-start gap-3">
      <AlertCircle size={16} class="shrink-0 mt-0.5" />
      <div class="flex-1">
        <p class="font-semibold">Unable to generate repository pulse metrics</p>
        <p class="mt-1 font-mono text-[11px] opacity-90">{error}</p>
        <button
          type="button"
          onclick={() => pulseStore.reload()}
          class="gp-btn !py-1 !px-2.5 mt-3 text-xs"
        >
          Try Again
        </button>
      </div>
    </div>
  {:else if loading && !report}
    <!-- Initial Loading Skeleton -->
    <div class="p-6 space-y-4">
      <div class="grid grid-cols-2 sm:grid-cols-4 gap-3">
        {#each Array(4) as _}
          <div class="h-20 bg-surface/60 rounded-xl border border-border animate-pulse"></div>
        {/each}
      </div>
      <div class="h-44 bg-surface/60 rounded-xl border border-border animate-pulse"></div>
      <div class="h-48 bg-surface/60 rounded-xl border border-border animate-pulse"></div>
    </div>
  {:else if report && report.commits.length === 0}
    <!-- Empty Repository State -->
    <div class="flex-1 flex items-center justify-center p-8">
      <EmptyState
        icon={GitCommit}
        title="No commit activity found"
        hint="This repository does not have any commits yet, or the history could not be reached."
        action={{
          label: "Refresh Pulse",
          onClick: () => pulseStore.reload(),
          icon: RefreshCw,
        }}
      />
    </div>
  {:else if report}
    <!-- Active Pulse Dashboard -->
    <div class="p-6 space-y-5 max-w-7xl">
      <!-- 1. Streaks & Rhythm -->
      <PulseRhythm commits={report.commits} />

      <!-- 2. Contribution Calendar -->
      <PulseHeatmap commits={report.commits} />

      <!-- 3. Line Changes Over Time & LOC -->
      <PulseLineTrend commits={report.commits} {totalLoc} />

      <!-- 4. Punch Card (After-Hours Burnout Tracker) -->
      <PulsePunchCard commits={report.commits} />

      <!-- 5. Commit Hygiene & Quality -->
      <PulseHygiene commits={report.commits} />

      <!-- 6. Top Hotspot Files by Churn -->
      {#if report.top_files_by_churn.length > 0}
        <div class="gp-card p-4 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col gap-3">
          <div class="flex items-center justify-between border-b border-border/50 pb-2.5">
            <div class="flex items-center gap-2">
              <FileText size={15} class="text-accent shrink-0" />
              <span class="text-xs font-semibold text-textPrimary uppercase tracking-wider">Top Churn Hotspots</span>
              <span class="text-[11px] text-textMuted">(Files with highest line change frequency)</span>
            </div>
            <span class="text-[11px] text-textMuted font-mono">Top {Math.min(10, report.top_files_by_churn.length)} files</span>
          </div>

          <div class="overflow-x-auto">
            <table class="w-full text-left text-xs font-sans">
              <thead>
                <tr class="text-[10px] text-textMuted uppercase border-b border-border/60">
                  <th class="pb-2 font-medium">Path</th>
                  <th class="pb-2 font-medium text-right">Commits</th>
                  <th class="pb-2 font-medium text-right">Additions</th>
                  <th class="pb-2 font-medium text-right">Deletions</th>
                  <th class="pb-2 font-medium text-right">Total Churn</th>
                </tr>
              </thead>
              <tbody class="divide-y border-t border-border/40 font-mono text-[11px]">
                {#each report.top_files_by_churn.slice(0, 10) as file}
                  <tr class="hover:bg-surface/80 transition-colors">
                    <td class="py-2 text-textPrimary truncate max-w-xs sm:max-w-md" title={file.path}>
                      {file.path}
                    </td>
                    <td class="py-2 text-right text-textMuted">{file.commits_count}</td>
                    <td class="py-2 text-right text-emerald-500">+{file.additions.toLocaleString()}</td>
                    <td class="py-2 text-right text-rose-500">-{file.deletions.toLocaleString()}</td>
                    <td class="py-2 text-right font-bold text-textPrimary">
                      {(file.additions + file.deletions).toLocaleString()}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        </div>
      {/if}
    </div>
  {/if}
</div>
