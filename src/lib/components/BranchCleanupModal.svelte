<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { fade, scale } from "svelte/transition";
  import {
    backdropFade,
    backdropFadeOut,
    cardScale,
    cardScaleOut,
  } from "../ui/transitions";
  import {
    deadbranchClean,
    deadbranchListBackups,
    deadbranchRestore,
    deadbranchScan,
    filterStaleBranches,
    formatBackupTimestamp,
    formatBranchAge,
    severityBadge,
  } from "../branches/deadbranch";
  import type {
    DeadbranchBackupInfo,
    DeadbranchScanResult,
  } from "../branches/types";
  import {
    AlertCircle,
    Archive,
    Check,
    Clock,
    GitBranch,
    GitMerge,
    RefreshCw,
    RotateCcw,
    Search,
    Shield,
    Trash2,
    X,
  } from "@lucide/svelte";

  let {
    isOpen = false,
    onClose,
  }: {
    isOpen?: boolean;
    onClose?: () => void;
  } = $props();

  let activeTab = $state<"clean" | "backups">("clean");
  let loading = $state(false);
  let actionInProgress = $state(false);
  let scanResult = $state<DeadbranchScanResult | null>(null);
  let backups = $state<DeadbranchBackupInfo[]>([]);
  let selectedNames = $state<Set<string>>(new Set());

  // Filter state
  let query = $state("");
  let minDays = $state(30);
  let mergedOnly = $state(true);
  let checkSquash = $state(true);
  let localOnly = $state(true);
  let createBackup = $state(true);
  let forceDelete = $state(false);
  let actionBanner = $state<{ text: string; kind: "ok" | "err"; backupPath?: string } | null>(null);

  const displayedBranches = $derived.by(() => {
    if (!scanResult) return [];
    return filterStaleBranches(
      scanResult.branches,
      query,
      minDays,
      mergedOnly,
      localOnly,
    );
  });

  const selectableBranches = $derived(
    displayedBranches.filter((b) => !b.is_protected && !b.is_current_or_worktree),
  );

  const allSelected = $derived(
    selectableBranches.length > 0 &&
      selectableBranches.every((b) => selectedNames.has(b.name)),
  );

  async function runScan() {
    if (!$repoStore.currentPath) return;
    loading = true;
    actionBanner = null;
    try {
      const res = await deadbranchScan($repoStore.currentPath, {
        days_threshold: minDays,
        merged_only: mergedOnly,
        check_squash: checkSquash,
        include_remote: !localOnly,
      });
      scanResult = res;

      // By default select all un-protected, non-worktree candidate branches that are merged
      const nextSelected = new Set<string>();
      for (const b of res.branches) {
        if (!b.is_protected && !b.is_current_or_worktree && b.is_merged) {
          nextSelected.add(b.name);
        }
      }
      selectedNames = nextSelected;
    } catch (err: unknown) {
      actionBanner = {
        text: `Scan failed: ${err instanceof Error ? err.message : String(err)}`,
        kind: "err",
      };
    } finally {
      loading = false;
    }
  }

  async function loadBackups() {
    if (!$repoStore.currentPath) return;
    try {
      backups = await deadbranchListBackups($repoStore.currentPath);
    } catch {
      backups = [];
    }
  }

  $effect(() => {
    if (isOpen && $repoStore.currentPath) {
      void runScan();
      void loadBackups();
    }
  });

  function toggleBranch(name: string) {
    const next = new Set(selectedNames);
    if (next.has(name)) {
      next.delete(name);
    } else {
      next.add(name);
    }
    selectedNames = next;
  }

  function toggleAll() {
    if (allSelected) {
      selectedNames = new Set();
    } else {
      const next = new Set<string>();
      for (const b of selectableBranches) {
        next.add(b.name);
      }
      selectedNames = next;
    }
  }

  async function executeClean() {
    if (!$repoStore.currentPath || selectedNames.size === 0) return;
    actionInProgress = true;
    actionBanner = null;
    const branchesToClean = Array.from(selectedNames);

    try {
      const res = await deadbranchClean(
        $repoStore.currentPath,
        branchesToClean,
        forceDelete,
        createBackup,
      );

      actionBanner = {
        text: res.message,
        kind: res.failed.length === 0 ? "ok" : "err",
        backupPath: res.backup_path ?? undefined,
      };

      if (res.deleted.length > 0) {
        toastStore.info(`Cleaned ${res.deleted.length} branch(es) safely.`);
        await repoStore.refresh();
        await runScan();
        await loadBackups();
      }
    } catch (err: unknown) {
      actionBanner = {
        text: `Clean failed: ${err instanceof Error ? err.message : String(err)}`,
        kind: "err",
      };
    } finally {
      actionInProgress = false;
    }
  }

  async function executeRestore(backupPath: string) {
    if (!$repoStore.currentPath) return;
    actionInProgress = true;
    actionBanner = null;
    try {
      const res = await deadbranchRestore($repoStore.currentPath, backupPath);
      actionBanner = {
        text: res.message,
        kind: res.failed.length === 0 ? "ok" : "err",
      };
      if (res.restored.length > 0) {
        toastStore.info(`Restored ${res.restored.length} branch(es).`);
        await repoStore.refresh();
        await runScan();
      }
    } catch (err: unknown) {
      actionBanner = {
        text: `Restore failed: ${err instanceof Error ? err.message : String(err)}`,
        kind: "err",
      };
    } finally {
      actionInProgress = false;
    }
  }
</script>

{#if isOpen}
  <!-- Backdrop -->
  <!-- Justified: the scrim is a pointer dismiss for the dialog. Escape and the close button are the keyboard path; the scrim itself is presentation, not a focus stop. -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="gp-scrim bg-black/40 flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL};"
    in:fade={backdropFade()}
    out:fade={backdropFadeOut()}
    onclick={onClose}
    role="presentation"
  >
    <!-- Card Container -->
    <!-- Justified: the click stops the scrim dismiss from firing when the pointer lands on the dialog. The dialog is focused by trapFocus; this handler activates nothing. -->
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_noninteractive_element_interactions -->
    <div
      class="w-full max-w-4xl gp-card shadow-float rounded-2xl flex flex-col max-h-[85vh] overflow-hidden focus:outline-hidden gp-gpu"
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      onclick={(e) => e.stopPropagation()}
      use:trapFocus
      role="dialog"
      tabindex="-1"
      aria-modal="true"
      aria-labelledby="deadbranch-title"
    >
      <!-- Modal Header -->
      <div class="px-6 py-4 border-b border-border flex items-center justify-between bg-surfaceSecondary/40 shrink-0">
        <div class="flex items-center gap-3">
          <div class="w-8 h-8 rounded-lg bg-emerald-500/10 text-emerald-400 flex items-center justify-center border border-emerald-500/20">
            <Trash2 size={16} />
          </div>
          <div>
            <h2 id="deadbranch-title" class="text-sm font-semibold text-textPrimary flex items-center gap-2">
              Clean Stale Branches
              <span class="text-[10px] font-mono px-1.5 py-0.5 rounded bg-accent/10 text-accent border border-accent/20">deadbranch</span>
            </h2>
            <p class="text-xs text-textMuted mt-0.5">
              Safely identify and remove stale or merged Git branches with automatic backups and squash-merge detection.
            </p>
          </div>
        </div>

        <div class="flex items-center gap-2">
          <!-- Tab Navigation -->
          <div class="flex items-center bg-background rounded-lg p-0.5 border border-border text-xs">
            <button
              type="button"
              class="px-3 py-1 rounded-md font-medium transition-colors {activeTab === 'clean' ? 'bg-surface text-textPrimary shadow-xs' : 'text-textMuted hover:text-textPrimary'}"
              onclick={() => (activeTab = "clean")}
            >
              Scan & Clean
            </button>
            <button
              type="button"
              class="px-3 py-1 rounded-md font-medium transition-colors flex items-center gap-1.5 {activeTab === 'backups' ? 'bg-surface text-textPrimary shadow-xs' : 'text-textMuted hover:text-textPrimary'}"
              onclick={() => (activeTab = "backups")}
            >
              <Archive size={12} />
              Backups ({backups.length})
            </button>
          </div>

          <button
            type="button"
            class="p-1.5 text-textMuted hover:text-textPrimary rounded-lg hover:bg-surfaceHover transition-colors ml-2"
            onclick={onClose}
            aria-label="Close dialog"
          >
            <X size={16} />
          </button>
        </div>
      </div>

      <!-- Action Banner -->
      {#if actionBanner}
        <div
          class="px-6 py-2.5 text-xs flex items-center justify-between border-b {actionBanner.kind === 'ok' ? 'bg-emerald-500/10 text-emerald-300 border-emerald-500/20' : 'bg-rose-500/10 text-rose-300 border-rose-500/20'}"
        >
          <div class="flex items-center gap-2">
            {#if actionBanner.kind === 'ok'}
              <Check size={14} class="text-emerald-400 shrink-0" />
            {:else}
              <AlertCircle size={14} class="text-rose-400 shrink-0" />
            {/if}
            <span>{actionBanner.text}</span>
          </div>

          {#if actionBanner.backupPath}
            <button
              type="button"
              class="text-[11px] underline font-medium hover:text-textPrimary cursor-pointer ml-4"
              onclick={() => void executeRestore(actionBanner!.backupPath!)}
            >
              Undo / Restore Branches
            </button>
          {/if}
        </div>
      {/if}

      {#if activeTab === "clean"}
        <!-- Scan Metrics Header -->
        <div class="grid grid-cols-4 gap-3 px-6 py-3 bg-surface border-b border-border text-xs shrink-0">
          <div class="flex flex-col">
            <span class="text-[10px] uppercase tracking-wider text-textMuted">Branches Scanned</span>
            <span class="text-base font-semibold text-textPrimary mt-0.5">{scanResult?.total_scanned ?? "—"}</span>
          </div>
          <div class="flex flex-col">
            <span class="text-[10px] uppercase tracking-wider text-textMuted">Stale (&gt;{minDays}d)</span>
            <span class="text-base font-semibold text-amber-400 mt-0.5">{scanResult?.stale_count ?? "—"}</span>
          </div>
          <div class="flex flex-col">
            <span class="text-[10px] uppercase tracking-wider text-textMuted">Total Merged</span>
            <span class="text-base font-semibold text-emerald-400 mt-0.5">{scanResult?.merged_count ?? "—"}</span>
          </div>
          <div class="flex flex-col">
            <span class="text-[10px] uppercase tracking-wider text-textMuted flex items-center gap-1">
              Squash-Merged
              <span class="text-[9px] text-accent font-mono" title="Detected via git merge-tree">merge-tree</span>
            </span>
            <span class="text-base font-semibold text-emerald-300 mt-0.5">{scanResult?.squash_merged_count ?? "—"}</span>
          </div>
        </div>

        <!-- Filter Controls -->
        <div class="px-6 py-3 border-b border-border bg-surfaceSecondary/20 flex flex-wrap items-center gap-4 text-xs shrink-0">
          <!-- Search input -->
          <div class="relative flex-1 min-w-44">
            <Search size={13} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted" />
            <input
              type="text"
              bind:value={query}
              placeholder="Filter branches or authors..."
              class="w-full bg-background border border-border rounded-lg pl-8 pr-3 py-1.5 text-xs text-textPrimary placeholder:text-textMuted focus:outline-hidden focus:border-accent"
            />
          </div>

          <!-- Days Slider / Number -->
          <div class="flex items-center gap-2">
            <span class="text-textMuted">Min age:</span>
            <input
              type="number"
              min="0"
              max="365"
              bind:value={minDays}
              onchange={() => void runScan()}
              class="w-16 bg-background border border-border rounded-lg px-2 py-1 text-xs text-center text-textPrimary focus:outline-hidden focus:border-accent"
            />
            <span class="text-textMuted">days</span>
          </div>

          <!-- Toggles -->
          <label class="flex items-center gap-1.5 cursor-pointer text-textPrimary select-none">
            <input
              type="checkbox"
              bind:checked={mergedOnly}
              onchange={() => void runScan()}
              class="rounded border-border text-accent focus:ring-0"
            />
            <span>Merged only</span>
          </label>

          <label class="flex items-center gap-1.5 cursor-pointer text-textPrimary select-none" title="Use git merge-tree to check if changes were incorporated via GitHub/GitLab squash merge">
            <input
              type="checkbox"
              bind:checked={checkSquash}
              onchange={() => void runScan()}
              class="rounded border-border text-accent focus:ring-0"
            />
            <span>Check squash-merges</span>
          </label>

          <label class="flex items-center gap-1.5 cursor-pointer text-textPrimary select-none">
            <input
              type="checkbox"
              bind:checked={localOnly}
              onchange={() => void runScan()}
              class="rounded border-border text-accent focus:ring-0"
            />
            <span>Local only</span>
          </label>

          <button
            type="button"
            class="px-2.5 py-1.5 rounded-lg bg-surfaceHover hover:bg-border text-textPrimary flex items-center gap-1.5 transition-colors ml-auto"
            onclick={() => void runScan()}
            disabled={loading}
          >
            <RefreshCw size={12} class={loading ? "animate-spin text-accent" : ""} />
            <span>Rescan</span>
          </button>
        </div>

        <!-- Branch Table / List -->
        <div class="flex-1 overflow-y-auto min-h-60 max-h-96 divide-y divide-border/60">
          {#if loading}
            <div class="p-12 flex flex-col items-center justify-center text-textMuted gap-3">
              <RefreshCw size={24} class="animate-spin text-accent" />
              <p class="text-xs">Analyzing repository branches & squash-merges...</p>
            </div>
          {:else if displayedBranches.length === 0}
            <div class="p-12 flex flex-col items-center justify-center text-textMuted gap-2">
              <Shield size={24} class="text-emerald-400" />
              <p class="text-sm font-medium text-textPrimary">No candidate stale branches found</p>
              <p class="text-xs">Your repository branch hygiene is clean matching current filters.</p>
            </div>
          {:else}
            <!-- Table Header -->
            <div class="sticky top-0 bg-surfaceSecondary/90 backdrop-blur-xs px-6 py-2 flex items-center gap-3 text-[11px] font-semibold text-textMuted border-b border-border z-10">
              <input
                type="checkbox"
                checked={allSelected}
                onchange={toggleAll}
                class="rounded border-border text-accent focus:ring-0"
                aria-label="Select all candidate branches"
              />
              <span class="flex-1">Branch</span>
              <span class="w-32">Merge Status</span>
              <span class="w-28">Age</span>
              <span class="w-36">Last Commit Author</span>
            </div>

            {#each displayedBranches as branch (branch.name)}
              {@const isSelected = selectedNames.has(branch.name)}
              {@const badge = severityBadge(branch.severity as any)}
              {@const disabled = branch.is_protected || branch.is_current_or_worktree}

              <div
                class="px-6 py-2.5 flex items-center gap-3 text-xs hover:bg-surfaceHover/50 transition-colors {disabled ? 'opacity-50 bg-surfaceSecondary/40' : ''}"
              >
                <input
                  type="checkbox"
                  checked={isSelected}
                  {disabled}
                  onchange={() => toggleBranch(branch.name)}
                  class="rounded border-border text-accent focus:ring-0"
                />

                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-2">
                    <GitBranch size={13} class="text-accent shrink-0" />
                    <span class="font-medium text-textPrimary truncate">{branch.short_name}</span>
                    {#if branch.is_remote}
                      <span class="text-[10px] px-1.5 py-0.2 rounded bg-surface text-textMuted border border-border">remote</span>
                    {/if}
                    {#if branch.is_protected}
                      <span class="text-[10px] px-1.5 py-0.2 rounded bg-zinc-500/10 text-zinc-400 border border-zinc-500/20">protected</span>
                    {/if}
                    {#if branch.is_current_or_worktree}
                      <span class="text-[10px] px-1.5 py-0.2 rounded bg-amber-500/10 text-amber-400 border border-amber-500/20">worktree in-use</span>
                    {/if}
                    {#if branch.is_wip}
                      <span class="text-[10px] px-1.5 py-0.2 rounded bg-purple-500/10 text-purple-400 border border-purple-500/20">WIP</span>
                    {/if}
                  </div>
                  {#if branch.last_summary}
                    <p class="text-[11px] text-textMuted truncate mt-0.5">{branch.last_summary}</p>
                  {/if}
                </div>

                <!-- Merge Status Column -->
                <div class="w-32 shrink-0 flex items-center gap-1.5">
                  {#if branch.merged_by_tree}
                    <span class="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-medium bg-emerald-500/10 text-emerald-300 border border-emerald-500/20" title="All changes incorporated into trunk via squash/rebase merge">
                      <GitMerge size={10} />
                      Squash-merged
                    </span>
                  {:else if branch.is_merged}
                    <span class="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-medium bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
                      <Check size={10} />
                      Merged
                    </span>
                  {:else}
                    <span class="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-medium bg-amber-500/10 text-amber-400 border border-amber-500/20">
                      Unmerged
                    </span>
                  {/if}
                </div>

                <!-- Age Column -->
                <div class="w-28 shrink-0 flex items-center gap-1.5">
                  <span class="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-medium {badge.bgClass} {badge.textClass} {badge.borderClass}">
                    <Clock size={10} />
                    {formatBranchAge(branch.age_days)}
                  </span>
                </div>

                <!-- Author Column -->
                <div class="w-36 shrink-0 truncate text-textMuted">
                  {branch.last_author || "Unknown"}
                </div>
              </div>
            {/each}
          {/if}
        </div>

        <!-- Footer / Clean Execution Bar -->
        <div class="px-6 py-4 border-t border-border bg-surfaceSecondary/40 flex items-center justify-between gap-4 text-xs shrink-0">
          <div class="flex items-center gap-4">
            <label class="flex items-center gap-2 cursor-pointer text-textPrimary select-none">
              <input
                type="checkbox"
                bind:checked={createBackup}
                class="rounded border-border text-accent focus:ring-0"
              />
              <span>Create backup file before deletion</span>
            </label>

            <label class="flex items-center gap-2 cursor-pointer text-amber-400 select-none" title="Bypass git's -d refusal for unmerged branches">
              <input
                type="checkbox"
                bind:checked={forceDelete}
                class="rounded border-border text-amber-500 focus:ring-0"
              />
              <span>Force delete (-D)</span>
            </label>
          </div>

          <div class="flex items-center gap-3">
            <span class="text-textMuted font-medium">
              {selectedNames.size} of {selectableBranches.length} selected
            </span>

            <button
              type="button"
              class="px-4 py-2 rounded-xl font-medium bg-rose-500 hover:bg-rose-600 active:bg-rose-700 text-white flex items-center gap-2 transition-colors disabled:opacity-50 disabled:cursor-not-allowed shadow-sm"
              onclick={() => void executeClean()}
              disabled={selectedNames.size === 0 || actionInProgress}
            >
              <Trash2 size={13} />
              <span>{actionInProgress ? "Cleaning..." : `Clean Selected (${selectedNames.size})`}</span>
            </button>
          </div>
        </div>

      {:else}
        <!-- Backups List View -->
        <div class="flex-1 overflow-y-auto min-h-72 p-6 divide-y divide-border/60">
          {#if backups.length === 0}
            <div class="py-12 flex flex-col items-center justify-center text-textMuted gap-2">
              <Archive size={24} class="text-textMuted" />
              <p class="text-sm font-medium text-textPrimary">No branch backups found</p>
              <p class="text-xs">Backups are automatically created whenever branches are cleaned.</p>
            </div>
          {:else}
            <div class="mb-4">
              <h3 class="text-xs font-semibold text-textPrimary">Saved Branch Backups</h3>
              <p class="text-[11px] text-textMuted">Backups contain exact branch tips and SHAs for instant restoration.</p>
            </div>

            {#each backups as b (b.path)}
              <div class="py-3 flex items-center justify-between gap-4">
                <div class="flex items-center gap-3 min-w-0">
                  <div class="w-8 h-8 rounded-lg bg-surface flex items-center justify-center text-accent border border-border shrink-0">
                    <Archive size={14} />
                  </div>
                  <div class="min-w-0">
                    <p class="text-xs font-medium text-textPrimary flex items-center gap-2">
                      <span>{b.filename}</span>
                      <span class="text-[10px] px-1.5 py-0.5 rounded bg-surface text-textMuted border border-border">
                        {b.branch_count} branch{b.branch_count === 1 ? "" : "es"}
                      </span>
                    </p>
                    <p class="text-[11px] text-textMuted truncate mt-0.5">
                      Created {formatBackupTimestamp(b.timestamp)} · <span class="font-mono">{b.path}</span>
                    </p>
                  </div>
                </div>

                <button
                  type="button"
                  class="px-3 py-1.5 rounded-lg border border-border hover:bg-surfaceHover text-textPrimary flex items-center gap-1.5 text-xs transition-colors shrink-0 disabled:opacity-50"
                  onclick={() => void executeRestore(b.path)}
                  disabled={actionInProgress}
                >
                  <RotateCcw size={12} class="text-accent" />
                  <span>Restore All</span>
                </button>
              </div>
            {/each}
          {/if}
        </div>
      {/if}
    </div>
  </div>
{/if}
