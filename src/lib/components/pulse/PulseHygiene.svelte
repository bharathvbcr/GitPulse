<script lang="ts">
  import { computeHygiene } from "../../pulse/metrics";
  import type { PulseCommitSummary } from "../../pulse/types";
  import { CheckCircle2, GitMerge, KeyRound, Scale, Users } from "lucide-svelte";

  let {
    commits = [],
  }: {
    commits: readonly PulseCommitSummary[];
  } = $props();

  const hygiene = $derived(computeHygiene(commits));
</script>

<div class="grid grid-cols-2 sm:grid-cols-5 gap-3">
  <!-- Conventional Commits -->
  <div class="gp-card p-3.5 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col justify-between">
    <div class="flex items-center justify-between text-textMuted text-[11px] mb-1">
      <span class="font-medium uppercase tracking-wider">Conventional</span>
      <CheckCircle2 size={14} class="text-accent shrink-0" />
    </div>
    <div class="flex items-baseline gap-1.5 mt-1">
      <span class="text-2xl font-bold tracking-tight text-textPrimary">{hygiene.conventionalPercentage}%</span>
    </div>
    <div class="text-[10px] text-textMuted mt-1 truncate">
      {hygiene.conventionalCount} of {hygiene.totalCommits} standard
    </div>
  </div>

  <!-- Median Commit Size -->
  <div class="gp-card p-3.5 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col justify-between">
    <div class="flex items-center justify-between text-textMuted text-[11px] mb-1">
      <span class="font-medium uppercase tracking-wider">Median Churn</span>
      <Scale size={14} class="text-accent shrink-0" />
    </div>
    <div class="flex items-baseline gap-1.5 mt-1">
      <span class="text-2xl font-bold tracking-tight text-textPrimary">{hygiene.medianChurn}</span>
      <span class="text-xs text-textMuted font-normal">lines</span>
    </div>
    <div class="text-[10px] text-textMuted mt-1 truncate">
      Typical patch size
    </div>
  </div>

  <!-- Signed Commits -->
  <div class="gp-card p-3.5 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col justify-between">
    <div class="flex items-center justify-between text-textMuted text-[11px] mb-1">
      <span class="font-medium uppercase tracking-wider">Signed (GPG)</span>
      <KeyRound size={14} class="text-accent shrink-0" />
    </div>
    <div class="flex items-baseline gap-1.5 mt-1">
      <span class="text-2xl font-bold tracking-tight text-textPrimary">{hygiene.signedPercentage}%</span>
    </div>
    <div class="text-[10px] text-textMuted mt-1 truncate">
      {hygiene.signedCount} verified signatures
    </div>
  </div>

  <!-- Merge Commit % -->
  <div class="gp-card p-3.5 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col justify-between">
    <div class="flex items-center justify-between text-textMuted text-[11px] mb-1">
      <span class="font-medium uppercase tracking-wider">Merge Commits</span>
      <GitMerge size={14} class="text-accent shrink-0" />
    </div>
    <div class="flex items-baseline gap-1.5 mt-1">
      <span class="text-2xl font-bold tracking-tight text-textPrimary">{hygiene.mergePercentage}%</span>
    </div>
    <div class="text-[10px] text-textMuted mt-1 truncate">
      {hygiene.mergeCount} branch merges
    </div>
  </div>

  <!-- Co-authored Rate -->
  <div class="gp-card p-3.5 rounded-xl border border-border/80 bg-surface/50 shadow-sm flex flex-col justify-between col-span-2 sm:col-span-1">
    <div class="flex items-center justify-between text-textMuted text-[11px] mb-1">
      <span class="font-medium uppercase tracking-wider">Co-Authored</span>
      <Users size={14} class="text-accent shrink-0" />
    </div>
    <div class="flex items-baseline gap-1.5 mt-1">
      <span class="text-2xl font-bold tracking-tight text-textPrimary">{hygiene.coAuthorPercentage}%</span>
    </div>
    <div class="text-[10px] text-textMuted mt-1 truncate">
      Pair programming trailers
    </div>
  </div>
</div>
