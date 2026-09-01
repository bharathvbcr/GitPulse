<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import CommitCadence from "./CommitCadence.svelte";
  import {
    GitBranch,
    RefreshCw,
    AlertTriangle,
    FileDiff,
    ArrowUp,
    ArrowDown,
    Keyboard,
    CheckCircle2,
  } from "lucide-svelte";

  let {
    onOpenShortcuts,
  }: {
    onOpenShortcuts?: () => void;
  } = $props();

  let currentBranch = $derived($repoStore.currentBranch || "HEAD");
  let branchInfo = $derived(
    $repoStore.branches.find((b) => b.is_current || b.name === currentBranch)
  );

  let dirtyCount = $derived($repoStore.statuses.length);
  let stagedCount = $derived($repoStore.statuses.filter((s) => s.is_staged).length);
  let conflictedCount = $derived($repoStore.statuses.filter((s) => s.is_conflicted).length);

  let aheadCount = $derived(branchInfo?.ahead_count ?? 0);
  let behindCount = $derived(branchInfo?.behind_count ?? 0);

  function openShortcuts() {
    if (onOpenShortcuts) {
      onOpenShortcuts();
    } else {
      window.dispatchEvent(new CustomEvent("gitpulse:shortcuts"));
    }
  }

  function openCommandPalette() {
    window.dispatchEvent(new CustomEvent("gitpulse:palette"));
  }
</script>

<footer
  class="h-6 shrink-0 bg-surface/95 border-t border-border/70 px-3 flex items-center justify-between text-[11px] font-sans text-textMuted select-none gp-gpu z-20"
  role="status"
  aria-label="Repository Status Bar"
>
  <!-- Left Segment: Branch, Ahead/Behind, Dirty & Conflicts -->
  <div class="flex items-center gap-3 min-w-0">
    <!-- Branch Pill -->
    <div
      class="inline-flex items-center gap-1.5 font-medium text-textPrimary truncate"
      title="Current Branch: {currentBranch}"
    >
      <GitBranch size={12} class="text-accent shrink-0" />
      <span class="truncate max-w-[140px] font-mono text-[11px]">{currentBranch}</span>
    </div>

    <!-- Sync status (Ahead / Behind) -->
    {#if aheadCount > 0 || behindCount > 0}
      <div
        class="inline-flex items-center gap-1 font-mono text-[10px] text-textMuted px-1.5 py-0.2 rounded bg-surfaceHover border border-border/60"
        title="{aheadCount} commits ahead, {behindCount} commits behind upstream"
      >
        {#if aheadCount > 0}
          <span class="flex items-center text-emerald-600 dark:text-emerald-400">
            <ArrowUp size={10} />{aheadCount}
          </span>
        {/if}
        {#if behindCount > 0}
          <span class="flex items-center text-sky-600 dark:text-sky-400">
            <ArrowDown size={10} />{behindCount}
          </span>
        {/if}
      </div>
    {/if}

    <span class="text-border">|</span>

    <!-- Working Tree Changes / Dirty Files -->
    {#if dirtyCount > 0}
      <button
        type="button"
        onclick={() => repoStore.setActiveTab("diff")}
        class="inline-flex items-center gap-1 text-textMuted hover:text-textPrimary transition-colors"
        title="View {dirtyCount} changed file{dirtyCount === 1 ? '' : 's'} ({stagedCount} staged)"
      >
        <FileDiff size={12} class="text-amber-500 shrink-0" />
        <span>{dirtyCount} modified</span>
        {#if stagedCount > 0}
          <span class="text-[10px] text-emerald-600 dark:text-emerald-400 font-mono">
            ({stagedCount} staged)
          </span>
        {/if}
      </button>
    {:else}
      <span class="inline-flex items-center gap-1 text-textMuted/70">
        <CheckCircle2 size={11} class="text-emerald-500/80" />
        <span>Clean</span>
      </span>
    {/if}

    <!-- Conflicts Indicator -->
    {#if conflictedCount > 0}
      <button
        type="button"
        onclick={() => repoStore.setActiveTab("conflict")}
        class="inline-flex items-center gap-1 font-semibold text-rose-600 dark:text-rose-400 hover:brightness-110 transition-colors animate-pulse"
        title="{conflictedCount} unresolved conflict{conflictedCount === 1 ? '' : 's'}"
      >
        <AlertTriangle size={12} class="shrink-0" />
        <span>{conflictedCount} conflict{conflictedCount === 1 ? '' : 's'}</span>
      </button>
    {/if}
  </div>

  <!-- Center Segment: Background Activity -->
  <div class="hidden sm:flex items-center gap-2 text-[10px]">
    <!-- Cadence reads the commits already loaded for the graph, so it costs
         no additional fetch. Hidden while syncing, when the window would be
         drawn from a partially loaded history. -->
    {#if !$repoStore.isLoading && $graphStore.commits.length > 0}
      <CommitCadence commits={$graphStore.commits} days={30} />
    {/if}
    {#if $repoStore.isLoading}
      <div class="flex items-center gap-1.5 text-accent font-medium">
        <RefreshCw size={10} class="animate-spin shrink-0" />
        <span>Syncing…</span>
      </div>
    {/if}
  </div>

  <!-- Right Segment: Quick Shortcuts -->
  <div class="flex items-center gap-3">
    <button
      type="button"
      onclick={openCommandPalette}
      class="inline-flex items-center gap-1 text-textMuted hover:text-textPrimary transition-colors text-[10px]"
      title="Open Command Palette"
    >
      <span class="gp-keycap">⌘K</span>
      <span class="hidden md:inline">Palette</span>
    </button>

    <button
      type="button"
      onclick={openShortcuts}
      class="inline-flex items-center gap-1 text-textMuted hover:text-textPrimary transition-colors text-[10px]"
      title="Open Keyboard Shortcuts"
    >
      <Keyboard size={11} class="shrink-0" />
      <span class="gp-keycap">?</span>
      <span class="hidden md:inline">Shortcuts</span>
    </button>
  </div>
</footer>
