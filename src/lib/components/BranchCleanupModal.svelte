<script lang="ts">
  import { untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import { reportFailure } from "../stores/mutationOutcome";
  import type { MutationTarget } from "../repos/storeTypes";
  import { displayName } from "../repos/paths";
  import { trapFocus } from "../ui/focusTrap";
  import { reportPanelError } from "../diagnostics/report";
  import { LAYERS } from "../ui/layers";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { plural } from "../format";
  import Checkbox from "./Checkbox.svelte";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { fade, scale } from "svelte/transition";
  import {
    backdropFade,
    backdropFadeOut,
    cardScale,
    cardScaleOut,
  } from "../ui/transitions";
  import {
    deadbranchListBackups,
    deadbranchScan,
    filterStaleBranches,
    formatBackupTimestamp,
    formatBranchAge,
    preselectMerged,
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
  /** Why the backup list could not be read; null while it was read (even if empty). */
  let backupsError = $state<string | null>(null);
  /** A read is in flight; Try again says so, since a repeated identical failure changes nothing else. */
  let backupsLoading = $state(false);
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

  /**
   * The repository each list on screen was read from, recorded when the list
   * landed. A clean or restore acts on THAT repository or on none: while a
   * rescan of the repository now in front is still out, the list is the
   * previous one's, and `currentPath` at click time is not its owner.
   */
  let scanTarget: MutationTarget | null = null;
  let backupsTarget: MutationTarget | null = null;
  // Latest request wins: a response that is not the newest is dropped.
  let scanSeq = 0;
  let backupsSeq = 0;

  /** "Couldn't <verb> <object>. <Reason>." for the banner. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  /** `owner` when it is still the repository in front; otherwise says so and returns null. */
  function liveTarget(owner: MutationTarget | null, what: string): MutationTarget | null {
    if (owner && repoStore.targetIsActive(owner)) return owner;
    toastStore.error(`${what}: the repository changed since this list was read. Rescan, then try again.`);
    return null;
  }

  /**
   * Whether the result belongs on screen. One for a repository that is no
   * longer in front is toasted under that repository's name instead: the
   * banner (and its Undo, which restores into whatever the backups list is
   * showing) is for the list on screen, and the list now is another's.
   * Returns true when the caller should go on and draw the banner.
   */
  function showsHere(owner: MutationTarget, message: string, backupPath: string | null | undefined): boolean {
    if (repoStore.targetIsActive(owner)) return true;
    toastStore.info(`${displayName(owner.path)}: ${message}${backupPath ? ` Backup: ${backupPath}` : ""}`);
    return false;
  }

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
    const target = repoStore.captureTarget();
    if (!target) return;
    const seq = ++scanSeq;
    loading = true;
    actionBanner = null;
    try {
      const res = await deadbranchScan(target.path, {
        days_threshold: minDays,
        merged_only: mergedOnly,
        check_squash: checkSquash,
        include_remote: !localOnly,
      });
      if (seq !== scanSeq) return;
      scanResult = res;
      scanTarget = target;

      // By default select the un-protected, non-worktree branches that are merged.
      // A gone upstream alone is not merged, so it is not pre-selected.
      selectedNames = preselectMerged(res.branches);
    } catch (err: unknown) {
      if (seq !== scanSeq) return;
      actionBanner = {
        text: failure("scan", "branches", err),
        kind: "err",
      };
    } finally {
      if (seq === scanSeq) loading = false;
    }
  }

  async function loadBackups() {
    const target = repoStore.captureTarget();
    if (!target) return;
    const seq = ++backupsSeq;
    backupsLoading = true;
    try {
      const listed = await deadbranchListBackups(target.path);
      if (seq !== backupsSeq) return;
      backups = listed;
      backupsTarget = target;
      backupsError = null;
    } catch (err: unknown) {
      if (seq !== backupsSeq) return;
      backups = [];
      backupsTarget = target;
      // An unreadable list is not an empty one: the tab is where deleted
      // branches are recovered, so "no backups" must never stand in for it.
      // Recorded and redacted like every other panel's read failure; the
      // modal opens from the Ops pane, so it reports as "ops".
      backupsError = reportPanelError("ops", err);
    } finally {
      if (seq === backupsSeq) backupsLoading = false;
    }
  }

  /**
   * Scan when the modal opens, or when the repository under it changes —
   * and at no other time.
   *
   * `$repoStore.currentPath` read here subscribed the effect to the whole
   * store, which publishes per commit-message keystroke and per ~6s status
   * poll: every publish rescanned and reset the reader's checkboxes to the
   * default selection. And `runScan` reads the filter state synchronously
   * before its first await, so those reads were tracked too — the filter
   * controls already rescan from their own `onchange`, so that was a second,
   * per-input scan. The path is a strictly-equal derived and the calls are
   * untracked, so only `isOpen` and the repository path drive this.
   */
  const scanRepoPath = $derived($currentRepoPath);
  $effect(() => {
    if (isOpen && scanRepoPath) {
      untrack(() => {
        void runScan();
        void loadBackups();
      });
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
    if (selectedNames.size === 0 || actionInProgress) return;
    const target = liveTarget(scanTarget, "Couldn't clean the branches");
    if (!target) return;
    actionInProgress = true;
    actionBanner = null;
    const branchesToClean = Array.from(selectedNames);

    try {
      const outcome = await repoStore.deadbranchClean(branchesToClean, forceDelete, createBackup, target);
      const res = outcome.output;
      if (!reportFailure(outcome, "Couldn't clean the branches") || !res) return;
      if (!showsHere(target, res.message, res.backup_path)) return;

      actionBanner = {
        text: res.message,
        kind: res.failed.length === 0 ? "ok" : "err",
        backupPath: res.backup_path ?? undefined,
      };

      if (res.deleted.length > 0) {
        toastStore.info(`Cleaned ${plural(res.deleted.length, "branch", "branches")}.`);
        await runScan();
        await loadBackups();
      }
    } catch (err: unknown) {
      actionBanner = {
        text: failure("clean", "branches", err),
        kind: "err",
      };
    } finally {
      actionInProgress = false;
    }
  }

  async function executeRestore(backupPath: string) {
    if (actionInProgress) return;
    const target = liveTarget(backupsTarget, "Couldn't restore the branches");
    if (!target) return;
    actionInProgress = true;
    actionBanner = null;
    try {
      const outcome = await repoStore.deadbranchRestore(backupPath, target);
      const res = outcome.output;
      if (!reportFailure(outcome, "Couldn't restore the branches") || !res) return;
      if (!showsHere(target, res.message, null)) return;
      actionBanner = {
        text: res.message,
        kind: res.failed.length === 0 ? "ok" : "err",
      };
      if (res.restored.length > 0) {
        toastStore.info(`Restored ${plural(res.restored.length, "branch", "branches")}.`);
        await runScan();
      }
    } catch (err: unknown) {
      actionBanner = {
        text: failure("restore", "branches", err),
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
      <div class="px-6 py-4 flex items-center justify-between bg-surfaceSecondary/40 shrink-0">
        <div class="flex items-center gap-3">
          <div class="w-8 h-8 rounded-lg bg-ok/10 text-ok flex items-center justify-center">
            <Trash2 size={16} />
          </div>
          <div>
            <h2 id="deadbranch-title" class="text-sm font-semibold text-textPrimary flex items-center gap-2">
              Clean stale branches
              <span class="gp-chip font-mono" data-tone="accent">deadbranch</span>
            </h2>
            <p class="text-xs text-textMuted mt-0.5">
              Safely identify and remove stale or merged Git branches with automatic backups and squash-merge detection.
            </p>
          </div>
        </div>

        <div class="flex items-center gap-2">
          <!-- Tab Navigation -->
          <div class="gp-segmented text-xs">
            <button
              type="button"
              class="gp-seg-btn"
              aria-pressed={activeTab === "clean"}
              data-active={activeTab === "clean" ? "true" : "false"}
              onclick={() => (activeTab = "clean")}
            >
              Scan & clean
            </button>
            <button
              type="button"
              class="gp-seg-btn flex items-center gap-1.5"
              aria-pressed={activeTab === "backups"}
              data-active={activeTab === "backups" ? "true" : "false"}
              onclick={() => (activeTab = "backups")}
            >
              <Archive size={12} />
              Backups ({backupsError ? "?" : backups.length})
            </button>
          </div>

          <button
            type="button"
            class="gp-icon-btn ml-2"
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
          class="gp-notice mx-0! mt-0! rounded-none! flex items-center justify-between"
          data-tone={actionBanner.kind === "ok" ? "ok" : "danger"}
          role={actionBanner.kind === "ok" ? "status" : "alert"}
        >
          <div class="flex items-center gap-2">
            {#if actionBanner.kind === 'ok'}
              <Check size={14} class="shrink-0" style="color: var(--gp-notice-ink)" />
            {:else}
              <AlertCircle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" />
            {/if}
            <span>{actionBanner.text}</span>
          </div>

          {#if actionBanner.backupPath}
            <button
              type="button"
              class="gp-btn shrink-0 ml-4"
              data-size="sm"
              onclick={() => void executeRestore(actionBanner!.backupPath!)}
            >
              Undo and restore branches
            </button>
          {/if}
        </div>
      {/if}

      {#if activeTab === "clean"}
        <!-- Scan Metrics Header -->
        <div class="grid grid-cols-4 gap-3 px-6 py-3 bg-surface text-xs shrink-0">
          <div class="flex flex-col">
            <span class="gp-label">Branches scanned</span>
            <span class="text-base font-semibold text-textPrimary mt-0.5">{scanResult?.total_scanned ?? "—"}</span>
          </div>
          <div class="flex flex-col">
            <span class="gp-label">Stale (&gt;{minDays}d)</span>
            <span class="text-base font-semibold text-warn mt-0.5">{scanResult?.stale_count ?? "—"}</span>
          </div>
          <div class="flex flex-col">
            <span class="gp-label">Total merged</span>
            <span class="text-base font-semibold text-ok mt-0.5">{scanResult?.merged_count ?? "—"}</span>
          </div>
          <div class="flex flex-col">
            <span class="gp-label flex items-center gap-1">
              Squash-merged
              <span class="text-micro text-accent font-mono" title="Detected via git merge-tree">merge-tree</span>
            </span>
            <span class="text-base font-semibold text-ok mt-0.5">{scanResult?.squash_merged_count ?? "—"}</span>
          </div>
        </div>

        <!-- Filter Controls -->
        <div class="px-6 py-3 bg-surfaceSecondary/20 flex flex-wrap items-center gap-4 text-xs shrink-0">
          <!-- Search input -->
          <div class="relative flex-1 min-w-44">
            <Search size={13} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted" />
            <input
              type="text"
              bind:value={query}
              placeholder="Filter branches or authors…"
              aria-label="Filter branches or authors"
              class="gp-field w-full pl-8"
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
              aria-label="Minimum age in days"
              class="gp-field w-16 text-center"
            />
            <span class="text-textMuted">days</span>
          </div>

          <!-- Toggles -->
          <Checkbox
            label="Merged only"
            checked={mergedOnly}
            onchange={(next) => {
              mergedOnly = next;
              void runScan();
            }}
          />

          <span title="Use git merge-tree to check if changes were incorporated via GitHub/GitLab squash merge">
            <Checkbox
              label="Check squash-merges"
              checked={checkSquash}
              onchange={(next) => {
                checkSquash = next;
                void runScan();
              }}
            />
          </span>

          <Checkbox
            label="Local only"
            checked={localOnly}
            onchange={(next) => {
              localOnly = next;
              void runScan();
            }}
          />

          <button
            type="button"
            class="gp-btn ml-auto"
            data-size="sm"
            onclick={() => void runScan()}
            disabled={loading}
          >
            <RefreshCw size={12} class={loading ? "animate-spin text-accent" : ""} />
            <span>Rescan</span>
          </button>
        </div>

        <!-- Branch Table / List -->
        <div class="flex-1 overflow-y-auto min-h-60 max-h-96 flex flex-col gap-0.5">
          {#if loading}
            <div class="p-6" aria-busy="true">
              <Skeleton variant="tree-row" count={5} />
              <p class="mt-3 text-center text-xs text-textMuted">Scanning branches and squash-merges…</p>
            </div>
          {:else if !scanResult && actionBanner}
            <!-- A scan that could not run is not a clean repository: say it could not be determined. -->
            <div class="p-6">
              <div class="gp-notice mx-0! mt-0! flex items-center gap-2" data-tone="unknown" role="status">
                <AlertCircle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" />
                <span class="min-w-0 flex-1">Couldn't scan branches, so none is shown as stale or clean.</span>
                <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={() => void runScan()}>Rescan</button>
              </div>
            </div>
          {:else if displayedBranches.length === 0}
            <EmptyState
              compact
              icon={Shield}
              title="No candidate stale branches found"
              hint="Nothing in this repository matches the current filters."
            />
          {:else}
            <!-- Table Header -->
            <div
              class="sticky top-0 bg-surfaceSecondary px-6 py-2 flex items-center gap-3 text-meta font-semibold text-textMuted"
              style="z-index: {LAYERS.STICKY};"
            >
              <Checkbox
                checked={allSelected}
                onchange={toggleAll}
                ariaLabel="Select all candidate branches"
              />
              <span class="flex-1">Branch</span>
              <span class="w-32">Merge status</span>
              <span class="w-28">Age</span>
              <span class="w-36">Last commit author</span>
            </div>

            {#each displayedBranches as branch (branch.name)}
              {@const isSelected = selectedNames.has(branch.name)}
              {@const badge = severityBadge(branch.severity)}
              {@const disabled = branch.is_protected || branch.is_current_or_worktree}

              <div
                class="px-6 py-2.5 flex items-center gap-3 text-xs hover:bg-surfaceHover/50 transition-colors {disabled ? 'opacity-50 bg-surfaceSecondary/40' : ''}"
              >
                <Checkbox
                  checked={isSelected}
                  {disabled}
                  onchange={() => toggleBranch(branch.name)}
                  ariaLabel="Select {branch.short_name}"
                />

                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-2">
                    <GitBranch size={13} class="text-accent shrink-0" />
                    <span class="font-medium text-textPrimary truncate">{branch.short_name}</span>
                    {#if branch.is_remote}
                      <span class="gp-chip" data-tone="neutral">remote</span>
                    {/if}
                    {#if branch.is_protected}
                      <span class="gp-chip" data-tone="neutral">protected</span>
                    {/if}
                    {#if branch.is_current_or_worktree}
                      <span class="gp-chip" data-tone="warn">worktree in-use</span>
                    {/if}
                    {#if branch.is_wip}
                      <span class="gp-chip" data-tone="info">WIP</span>
                    {/if}
                  </div>
                  {#if branch.last_summary}
                    <p class="text-meta text-textMuted truncate mt-0.5">{branch.last_summary}</p>
                  {/if}
                </div>

                <!-- Merge Status Column -->
                <div class="w-32 shrink-0 flex items-center gap-1.5">
                  {#if branch.merged_by_tree}
                    <span class="gp-chip" data-tone="ok" title="All changes incorporated into trunk via squash/rebase merge">
                      <GitMerge size={10} />
                      Squash-merged
                    </span>
                  {:else if branch.is_merged}
                    <span class="gp-chip" data-tone="ok">
                      <Check size={10} />
                      Merged
                    </span>
                  {:else}
                    <span class="gp-chip" data-tone="warn">
                      Unmerged
                    </span>
                  {/if}
                  {#if branch.upstream_gone && !branch.is_merged}
                    <span class="gp-chip" data-tone="danger" title="The upstream was deleted, but nothing shows this branch's commits reached the default branch">
                      Upstream gone
                    </span>
                  {/if}
                </div>

                <!-- Age Column -->
                <div class="w-28 shrink-0 flex items-center gap-1.5">
                  <span class="inline-flex items-center gap-1 px-2 py-0.5 rounded-full border text-micro font-medium {badge.bgClass} {badge.textClass} {badge.borderClass}">
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
        <div class="px-6 py-4 bg-surfaceSecondary/40 flex items-center justify-between gap-4 text-xs shrink-0">
          <div class="flex items-center gap-4">
            <Checkbox
              label="Create backup file before deletion"
              checked={createBackup}
              onchange={(next) => (createBackup = next)}
            />

            <Checkbox
              label="Force delete (-D)"
              description="Bypass git's -d refusal for unmerged branches."
              checked={forceDelete}
              onchange={(next) => (forceDelete = next)}
            />
          </div>

          <div class="flex items-center gap-3">
            <span class="text-textMuted font-medium">
              {selectedNames.size} of {selectableBranches.length} selected
            </span>

            <button
              type="button"
              class="gp-btn"
              data-variant="danger"
              data-size="md"
              onclick={() => void executeClean()}
              disabled={selectedNames.size === 0 || actionInProgress}
            >
              <Trash2 size={13} />
              <span>{actionInProgress ? "Cleaning…" : `Clean selected (${selectedNames.size})`}</span>
            </button>
          </div>
        </div>

      {:else}
        <!-- Backups List View -->
        <div class="flex-1 overflow-y-auto min-h-72 p-6 flex flex-col gap-1">
          {#if backupsError}
            <div role="alert" class="gp-notice mx-0! mt-0! flex flex-col items-center justify-center gap-2 py-8 text-center" data-tone="unknown">
              <AlertCircle size={24} style="color: var(--gp-notice-ink)" aria-hidden="true" />
              <p class="text-sm font-medium text-textPrimary">Couldn't read branch backups.</p>
              <p class="text-xs text-textMuted break-words max-w-md">{backupsError}</p>
              <button type="button" class="gp-btn mt-1" data-size="sm" disabled={backupsLoading} onclick={() => void loadBackups()}>
                <RefreshCw size={12} class={backupsLoading ? "animate-spin" : ""} /> {backupsLoading ? "Trying again…" : "Try again"}
              </button>
            </div>
          {:else if backups.length === 0}
            <EmptyState
              compact
              icon={Archive}
              title="No branch backups found"
              hint="Backups are created whenever branches are cleaned with the backup option on."
            />
          {:else}
            <div class="mb-4">
              <h3 class="text-xs font-semibold text-textPrimary">Saved branch backups</h3>
              <p class="text-meta text-textMuted">Backups contain exact branch tips and SHAs for instant restoration.</p>
            </div>

            {#each backups as b (b.path)}
              <div class="py-3 flex items-center justify-between gap-4">
                <div class="flex items-center gap-3 min-w-0">
                  <div class="w-8 h-8 rounded-lg bg-surfaceHover flex items-center justify-center text-accent shrink-0">
                    <Archive size={14} />
                  </div>
                  <div class="min-w-0">
                    <p class="text-xs font-medium text-textPrimary flex items-center gap-2">
                      <span>{b.filename}</span>
                      <span class="gp-chip" data-tone="neutral">
                        {b.branch_count} branch{b.branch_count === 1 ? "" : "es"}
                      </span>
                    </p>
                    <p class="text-meta text-textMuted truncate mt-0.5">
                      Created {formatBackupTimestamp(b.timestamp)} · <span class="font-mono">{b.path}</span>
                    </p>
                  </div>
                </div>

                <button
                  type="button"
                  class="gp-btn shrink-0"
                  data-size="sm"
                  onclick={() => void executeRestore(b.path)}
                  disabled={actionInProgress}
                >
                  <RotateCcw size={12} class="text-accent" />
                  <span>Restore all</span>
                </button>
              </div>
            {/each}
          {/if}
        </div>
      {/if}
    </div>
  </div>
{/if}
