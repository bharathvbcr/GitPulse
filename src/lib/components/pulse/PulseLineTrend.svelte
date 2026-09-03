<script lang="ts">
  import { computeLineChanges } from "../../pulse/metrics";
  import type { PulseCommitSummary, WeeklyLineBucket } from "../../pulse/types";
  import { ArrowDown, ArrowUp, TrendingUp } from "lucide-svelte";

  let {
    commits = [],
    totalLoc = 0,
    now = Date.now(),
  }: {
    commits: readonly PulseCommitSummary[];
    totalLoc?: number;
    now?: number;
  } = $props();

  const weeks = $derived(computeLineChanges(commits, 26, now));

  let hoveredBucket = $state<WeeklyLineBucket | null>(null);

  // Maximum value for scaling the stacked bars
  const maxWeeklyDelta = $derived(
    weeks.reduce((max, w) => Math.max(max, w.additions, w.deletions), 10),
  );

  // Overall totals
  const totalAdditions = $derived(commits.reduce((sum, c) => sum + c.additions, 0));
  const totalDeletions = $derived(commits.reduce((sum, c) => sum + c.deletions, 0));
  const totalNet = $derived(totalAdditions - totalDeletions);
</script>

<div class="gp-card p-4 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col gap-4">
  <div class="flex items-center justify-between border-b border-border/50 pb-2.5">
    <div class="flex items-center gap-2">
      <TrendingUp size={15} class="text-accent shrink-0" />
      <span class="text-xs font-semibold text-textPrimary uppercase tracking-wider">Line Changes & LOC History</span>
      <span class="text-[11px] text-textMuted">(Past 26 Weeks)</span>
    </div>

    <!-- Aggregate Net Churn Badge -->
    <div class="flex items-center gap-3 text-xs font-mono">
      <div class="flex items-center gap-1 text-emerald-500">
        <ArrowUp size={12} />
        <span>+{totalAdditions.toLocaleString()}</span>
      </div>
      <div class="flex items-center gap-1 text-rose-500">
        <ArrowDown size={12} />
        <span>-{totalDeletions.toLocaleString()}</span>
      </div>
      <div class="text-textPrimary font-semibold">
        Net {totalNet >= 0 ? `+${totalNet.toLocaleString()}` : totalNet.toLocaleString()}
      </div>
    </div>
  </div>

  <!-- Weekly Stacked Bar Chart -->
  <div class="flex flex-col gap-1.5">
    <div class="h-32 flex items-end gap-1.5 pt-4 pb-2 border-b border-border/40 select-none">
      {#each weeks as bucket (bucket.weekStart)}
        {@const addHeight = Math.min(100, Math.round((bucket.additions / maxWeeklyDelta) * 55))}
        {@const delHeight = Math.min(100, Math.round((bucket.deletions / maxWeeklyDelta) * 55))}

        <div
          role="img"
          aria-label="Week {bucket.weekStart}"
          onmouseenter={() => (hoveredBucket = bucket)}
          onmouseleave={() => (hoveredBucket = null)}
          class="flex-1 h-full flex flex-col justify-end items-center group relative cursor-pointer"
        >
          <!-- Center axis line -->
          <div class="w-full flex flex-col items-center">
            <!-- Additions (pointing up) -->
            <div
              class="w-full max-w-[12px] bg-emerald-500/80 rounded-t-[2px] transition-all group-hover:bg-emerald-400"
              style="height: {addHeight}px;"
            ></div>
            <div class="w-full h-px bg-border my-0.5"></div>
            <!-- Deletions (pointing down) -->
            <div
              class="w-full max-w-[12px] bg-rose-500/80 rounded-b-[2px] transition-all group-hover:bg-rose-400"
              style="height: {delHeight}px;"
            ></div>
          </div>
        </div>
      {/each}
    </div>

    <!-- Weeks Axis Labels -->
    <div class="flex justify-between text-[10px] text-textMuted px-1">
      <span>{weeks[0]?.weekStart ?? ''}</span>
      <span>Weekly Additions (green) & Deletions (red)</span>
      <span>{weeks[weeks.length - 1]?.weekStart ?? ''}</span>
    </div>
  </div>

  <!-- Footer Hover Info -->
  <div class="text-[11px] text-textMuted pt-1 flex items-center justify-between border-t border-border/40 min-h-5">
    {#if hoveredBucket}
      <div>
        <span class="font-medium text-textPrimary">Week of {hoveredBucket.weekStart}</span>:
        <span class="text-emerald-500 font-mono ml-1.5">+{hoveredBucket.additions.toLocaleString()}</span>
        <span class="text-rose-500 font-mono ml-1.5">-{hoveredBucket.deletions.toLocaleString()}</span>
        <span class="text-textPrimary font-mono ml-2">
          (Net {hoveredBucket.net >= 0 ? `+${hoveredBucket.net.toLocaleString()}` : hoveredBucket.net.toLocaleString()})
        </span>
      </div>
    {:else}
      <span>Hover over a weekly column for exact line delta details</span>
    {/if}

    {#if totalLoc > 0}
      <div class="text-textPrimary text-xs flex items-center gap-1.5">
        <span class="text-textMuted text-[11px]">Current Total LOC:</span>
        <span class="font-mono font-bold text-accent">{totalLoc.toLocaleString()}</span>
      </div>
    {/if}
  </div>
</div>
