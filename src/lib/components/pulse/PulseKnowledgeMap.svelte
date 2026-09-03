<script lang="ts">
  import type { KnowledgeReport } from "../../pulse/types";
  import { Users, AlertTriangle, ShieldCheck, FileWarning, RefreshCw } from "lucide-svelte";

  let {
    knowledge = null,
    loading = false,
    onRefresh,
  }: {
    knowledge: KnowledgeReport | null;
    loading?: boolean;
    onRefresh?: () => void;
  } = $props();

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

<div class="gp-card p-4 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col gap-4">
  <!-- Header -->
  <div class="flex items-center justify-between border-b border-border/40 pb-3">
    <div>
      <div class="flex items-center gap-2">
        <Users size={18} class="text-indigo-400" />
        <h3 class="text-sm font-semibold text-textPrimary">Knowledge Distribution & Bus Factor</h3>
      </div>
      <p class="text-xs text-textMuted mt-0.5">
        Line ownership and team concentration derived from git blame analysis.
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
        <span>{loading ? "Analyzing..." : "Re-scan Blame"}</span>
      </button>
    {/if}
  </div>

  {#if loading && !knowledge}
    <div class="py-12 text-center text-textMuted text-xs flex flex-col items-center gap-2">
      <RefreshCw size={20} class="animate-spin text-accent" />
      <span>Running parallel blame scanner across repository files...</span>
    </div>
  {:else if !knowledge || knowledge.scanned_files === 0}
    <div class="py-8 text-center text-textMuted text-xs">
      No blame knowledge data available.
    </div>
  {:else}
    <!-- Key Bus Factor & Scan Stat Row -->
    <div class="grid grid-cols-1 sm:grid-cols-3 gap-3">
      <!-- Bus Factor Card -->
      <div class="p-3 rounded-lg border border-border/60 bg-surface flex flex-col justify-between">
        <div class="flex items-center justify-between text-textMuted text-xs">
          <span class="font-medium uppercase tracking-wider text-[10px]">Bus Factor</span>
          {#if knowledge.bus_factor === 1}
            <span class="px-1.5 py-0.5 rounded text-[10px] font-bold bg-red-500/10 text-red-400 border border-red-500/20">
              Critical
            </span>
          {:else if knowledge.bus_factor === 2}
            <span class="px-1.5 py-0.5 rounded text-[10px] font-bold bg-amber-500/10 text-amber-400 border border-amber-500/20">
              Moderate
            </span>
          {:else}
            <span class="px-1.5 py-0.5 rounded text-[10px] font-bold bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
              Healthy
            </span>
          {/if}
        </div>
        <div class="flex items-baseline gap-2 mt-2">
          <span class="text-3xl font-extrabold {knowledge.bus_factor === 1 ? 'text-red-400' : knowledge.bus_factor === 2 ? 'text-amber-400' : 'text-emerald-400'}">
            {knowledge.bus_factor}
          </span>
          <span class="text-xs text-textMuted font-medium">key contributor{knowledge.bus_factor === 1 ? '' : 's'}</span>
        </div>
        <p class="text-[11px] text-textMuted mt-1">
          {knowledge.bus_factor === 1
            ? "1 contributor owns >= 50% of surviving code."
            : `${knowledge.bus_factor} contributors hold >= 50% of surviving code.`}
        </p>
      </div>

      <!-- Scanned Coverage -->
      <div class="p-3 rounded-lg border border-border/60 bg-surface flex flex-col justify-between">
        <div class="flex items-center justify-between text-textMuted text-xs">
          <span class="font-medium uppercase tracking-wider text-[10px]">Scanned Files</span>
          <ShieldCheck size={14} class="text-accent" />
        </div>
        <div class="flex items-baseline gap-2 mt-2">
          <span class="text-3xl font-extrabold text-textPrimary">{knowledge.scanned_files}</span>
          <span class="text-xs text-textMuted">/ {knowledge.candidate_files} candidate files</span>
        </div>
        <p class="text-[11px] text-textMuted mt-1">
          {knowledge.scanned_lines.toLocaleString()} live lines analyzed in {knowledge.duration_ms}ms
        </p>
      </div>

      <!-- Orphaned Files Count -->
      <div class="p-3 rounded-lg border border-border/60 bg-surface flex flex-col justify-between">
        <div class="flex items-center justify-between text-textMuted text-xs">
          <span class="font-medium uppercase tracking-wider text-[10px]">Orphaned Code</span>
          <AlertTriangle size={14} class={knowledge.orphaned_files.length > 0 ? "text-amber-400" : "text-emerald-400"} />
        </div>
        <div class="flex items-baseline gap-2 mt-2">
          <span class="text-3xl font-extrabold {knowledge.orphaned_files.length > 0 ? 'text-amber-400' : 'text-emerald-400'}">
            {knowledge.orphaned_files.length}
          </span>
          <span class="text-xs text-textMuted">stale files</span>
        </div>
        <p class="text-[11px] text-textMuted mt-1">
          Primary author inactive in repo for > 6 months
        </p>
      </div>
    </div>

    <!-- Team Authors Breakdown -->
    <div class="flex flex-col gap-2.5">
      <h4 class="text-xs font-semibold uppercase tracking-wider text-textMuted">Primary Code Ownership</h4>
      <div class="flex flex-col gap-2">
        {#each knowledge.primary_authors.slice(0, 8) as author}
          <div class="flex flex-col gap-1 p-2 rounded-lg bg-surface/60 border border-border/40">
            <div class="flex items-center justify-between text-xs">
              <div class="flex items-center gap-2">
                <span class="font-medium text-textPrimary">{author.author_name}</span>
                <span class="text-[10px] text-textMuted font-mono truncate max-w-xs">{author.author_email}</span>
              </div>
              <div class="flex items-center gap-2">
                <span class="text-textMuted text-[11px]">{author.lines_owned.toLocaleString()} lines</span>
                <span class="font-bold text-textPrimary text-xs">{author.percentage}%</span>
              </div>
            </div>
            <!-- Progress Bar -->
            <div class="w-full h-1.5 bg-surfaceMuted rounded-full overflow-hidden">
              <div
                class="h-full bg-accent rounded-full transition-all duration-500"
                style="width: {Math.min(100, author.percentage)}%;"
              ></div>
            </div>
          </div>
        {/each}
      </div>
    </div>

    <!-- Orphaned Files Table if present -->
    {#if knowledge.orphaned_files.length > 0}
      <div class="flex flex-col gap-2 mt-2">
        <div class="flex items-center gap-1.5 text-amber-400 text-xs font-semibold">
          <FileWarning size={14} />
          <h4>Orphaned Code Alert (> 180 days inactive owner)</h4>
        </div>
        <div class="overflow-x-auto max-h-48 overflow-y-auto border border-amber-500/20 rounded-lg">
          <table class="w-full text-left text-xs border-collapse">
            <thead class="bg-surfaceMuted text-textMuted uppercase text-[10px] tracking-wider sticky top-0">
              <tr class="border-b border-border/50">
                <th class="py-1.5 px-3">File Path</th>
                <th class="py-1.5 px-3">Primary Author</th>
                <th class="py-1.5 px-3">Live Lines</th>
                <th class="py-1.5 px-3 text-right">Last Owner Commit</th>
              </tr>
            </thead>
            <tbody class="divide-y divide-border/20">
              {#each knowledge.orphaned_files as file}
                <tr class="hover:bg-surfaceMuted/30 font-mono text-[11px]">
                  <td class="py-1.5 px-3 truncate max-w-xs text-textPrimary" title={file.path}>{file.path}</td>
                  <td class="py-1.5 px-3 text-textMuted font-sans">{file.primary_author}</td>
                  <td class="py-1.5 px-3 text-textMuted">{file.lines_count.toLocaleString()}</td>
                  <td class="py-1.5 px-3 text-right text-amber-400/90 font-sans">
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
      <div class="text-[11px] text-textMuted/80 italic flex items-center gap-1.5 border-t border-border/30 pt-2">
        <span>* Scan capped at {knowledge.scanned_files} candidate files to bound execution time. Large repositories retain honest bounds.</span>
      </div>
    {/if}
  {/if}
</div>
