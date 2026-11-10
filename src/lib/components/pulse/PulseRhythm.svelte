<script lang="ts">
  import { computeRhythm } from "../../pulse/metrics";
  import type { PulseCommitSummary } from "../../pulse/types";
  import { Calendar, Flame, Hourglass, Zap } from "@lucide/svelte";

  let {
    commits = [],
    now = Date.now(),
    truncated = false,
  }: {
    commits: readonly PulseCommitSummary[];
    now?: number;
    truncated?: boolean;
  } = $props();

  const rhythm = $derived(computeRhythm(commits, 90, now));
  const activeRate = $derived(
    rhythm.totalDaysInWindow > 0
      ? Math.round((rhythm.activeDaysInWindow / rhythm.totalDaysInWindow) * 100)
      : 0,
  );
</script>

<div class="grid grid-cols-2 sm:grid-cols-4 gap-3">
  <!-- Current Streak -->
  <div class="gp-card p-3.5 flex flex-col justify-between">
    <div class="flex items-center justify-between mb-1">
      <span class="gp-label">Current streak</span>
      <Zap size={14} class="text-accent shrink-0" />
    </div>
    <div class="flex items-baseline gap-1.5 mt-1">
      <span class="text-2xl font-semibold tracking-tight text-textPrimary">{rhythm.currentStreak}</span>
      <span class="text-xs text-textMuted font-normal">day{rhythm.currentStreak === 1 ? '' : 's'}</span>
    </div>
    <div class="text-micro text-textMuted mt-1 truncate">
      {rhythm.currentStreak > 0 ? 'Consecutive active days' : 'No commits today or yesterday'}
    </div>
  </div>

  <!-- Longest Streak -->
  <div class="gp-card p-3.5 flex flex-col justify-between">
    <div class="flex items-center justify-between mb-1">
      <span class="gp-label">Longest run</span>
      <Flame size={14} class="text-accent shrink-0" />
    </div>
    <div class="flex items-baseline gap-1.5 mt-1">
      <span class="text-2xl font-semibold tracking-tight text-textPrimary">{rhythm.longestStreak}</span>
      <span class="text-xs text-textMuted font-normal">day{rhythm.longestStreak === 1 ? '' : 's'}</span>
    </div>
    <div class="text-micro text-textMuted mt-1 truncate">
      Peak continuous cadence
    </div>
  </div>

  <!-- Active in 90 Days -->
  <div class="gp-card p-3.5 flex flex-col justify-between">
    <div class="flex items-center justify-between mb-1">
      <span class="gp-label">Active in 90 days</span>
      <Calendar size={14} class="text-accent shrink-0" />
    </div>
    <div class="flex items-baseline gap-1.5 mt-1">
      <span class="text-2xl font-semibold tracking-tight text-textPrimary">{rhythm.activeDaysInWindow}</span>
      <span class="text-xs text-textMuted font-normal">of {rhythm.totalDaysInWindow} days</span>
    </div>
    <div class="text-micro text-textMuted mt-1 flex items-center justify-between">
      <span>{activeRate}% active rate</span>
      <span class="w-10 h-1.5 bg-background rounded-full overflow-hidden inline-block ml-1">
        <span class="bg-accent h-full block" style="width: {activeRate}%"></span>
      </span>
    </div>
  </div>

  <!-- Longest Inactive Gap -->
  <div class="gp-card p-3.5 flex flex-col justify-between">
    <div class="flex items-center justify-between mb-1">
      <span class="gp-label">Longest gap</span>
      <Hourglass size={14} class="text-textMuted shrink-0" />
    </div>
    <div class="flex items-baseline gap-1.5 mt-1">
      <span class="text-2xl font-semibold tracking-tight text-textPrimary">{rhythm.longestInactiveGap}</span>
      <span class="text-xs text-textMuted font-normal">day{rhythm.longestInactiveGap === 1 ? '' : 's'}</span>
    </div>
    <div class="text-micro text-textMuted mt-1 truncate">
      Quiet stretch in the last 90 days
    </div>
  </div>
</div>
{#if truncated}
  <p class="text-meta text-textMuted -mt-1">
    Streak and gap are computed from the scanned window. Older history exists and is not shown as a longer gap.
  </p>
{/if}
