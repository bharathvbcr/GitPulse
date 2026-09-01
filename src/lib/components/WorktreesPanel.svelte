<script lang="ts">
  import type { WorktreeInfo } from "../branches/types";
  import { invoke } from "@tauri-apps/api/core";
  import { reportPanelError } from "../diagnostics/report";
  import { repoStore } from "../stores/repoStore";
  import { harnessStore } from "../stores/harnessStore";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import {
    FolderGit2,
    Plus,
    Trash2,
    ExternalLink,
    Lock,
    Unlock,
    Sparkles,
    AlertTriangle,
  } from "lucide-svelte";


  let worktrees = $state<WorktreeInfo[]>([]);
  let isLoading = $state(false);
  let error = $state<string | null>(null);
  let isCreating = $state(false);
  let removingPath = $state<string | null>(null);
  let showAddForm = $state(false);
  let newPath = $state("");
  let newBranch = $state("");
  let startPoint = $state("");
  let inflight: AsyncGuard | null = null;
  let confirmTimer: ReturnType<typeof setTimeout> | null = null;

  // Load trigger keyed on the real dependencies: active repo + its hydration
  // epoch. Memoized because every store emission re-runs the effect — an
  // unguarded rerun cancels in-flight loads and disarms the row-delete
  // confirm on each ~6s poll tick.
  let prevRepoPath: string | null = null;
  let prevGeneration: number | null = null;
  $effect(() => {
    const repo = $repoStore.currentPath;
    const generation = $repoStore.generation;
    if (repo === prevRepoPath && generation === prevGeneration) return;
    prevRepoPath = repo;
    prevGeneration = generation;
    void load();
    return () => {
      // A load that outlives its repo/generation must not apply: the next
      // effect run (or unmount) cancels it here, and load() re-checks below.
      inflight?.cancel();
      // Disarm any pending removal confirm; it must not leak across repos
      // and there is no teardown left that would otherwise fire per tick.
      if (confirmTimer !== null) clearTimeout(confirmTimer);
      confirmTimer = null;
      removingPath = null;
    };
  });

  async function load() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    isLoading = true;
    error = null;
    try {
      const next = await invoke<WorktreeInfo[]>("cmd_list_worktrees", { repoPath: repo });
      if (!guard.isLive()) return;
      worktrees = next;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      error = reportPanelError("worktrees", err);
    } finally {
      if (guard.isLive()) isLoading = false;
    }
  }

  async function toggleLock(wt: WorktreeInfo) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    try {
      if (wt.is_locked) {
        await invoke("cmd_unlock_worktree", { repoPath: repo, targetPath: wt.path });
      } else {
        await invoke("cmd_lock_worktree", { repoPath: repo, targetPath: wt.path, reason: null });
      }
      await load();
    } catch (err: unknown) {
      error = reportPanelError("worktrees", err);
    }
  }

  async function prune() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    try {
      await invoke("cmd_prune_worktree", { repoPath: repo });
      await load();
    } catch (err: unknown) {
      error = reportPanelError("worktrees", err);
    }
  }

  async function create() {
    const repo = $repoStore.currentPath;
    if (!repo || !newPath.trim()) return;
    isCreating = true;
    error = null;
    try {
      await invoke("cmd_add_worktree", {
        repoPath: repo,
        targetPath: newPath.trim(),
        newBranch: newBranch.trim() || null,
        startPoint: startPoint.trim() || null,
        detach: !newBranch.trim(),
      });
      if ($repoStore.currentPath !== repo) return;
      harnessStore.recordAction({
        kind: "worktree",
        label: newBranch.trim() ? `${newBranch.trim()} → ${newPath.trim()}` : newPath.trim(),
        ok: true,
      });
      newPath = "";
      newBranch = "";
      startPoint = "";
      showAddForm = false;
      await load();
    } catch (err: unknown) {
      if ($repoStore.currentPath !== repo) return;
      harnessStore.recordAction({ kind: "worktree", label: newPath.trim(), ok: false });
      error = reportPanelError("worktrees", err);
    } finally {
      isCreating = false;
    }
  }

  /** Armed-confirm label; names the discard cost when changes would be lost. */
  function removeArmTitle(wt: WorktreeInfo): string {
    if (removingPath !== wt.path) return "Remove this worktree";
    const dirty = wt.dirty_files ?? 0;
    return dirty > 0
      ? `Discard ${dirty} changed files? Click again to remove`
      : "Click again to remove";
  }

  async function remove(wt: WorktreeInfo) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    // Two-step confirm: the first click arms, the second removes. No native
    // dialog, so the flow stays keyboard-reachable and testable. The arming
    // timer is tracked so destroy (effect cleanup above) can clear it.
    if (removingPath !== wt.path) {
      removingPath = wt.path;
      if (confirmTimer !== null) clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => {
        confirmTimer = null;
        if (removingPath === wt.path) removingPath = null;
      }, 4000);
      return;
    }
    removingPath = null;
    // A dirty worktree must not be silently discarded: --force only when the
    // status scan found nothing to lose (dirty_files === null means "not
    // scanned", treated as clean so removal stays one confirm).
    const force = (wt.dirty_files ?? 0) === 0;
    try {
      await invoke("cmd_remove_worktree", { repoPath: repo, targetPath: wt.path, force });
      if ($repoStore.currentPath !== repo) return;
      harnessStore.recordAction({ kind: "worktree-remove", label: wt.path, ok: true });
      if ($repoStore.currentPath === wt.path) {
        // T-F09: the active tab points INTO the removed directory. Close it —
        // which activates a surviving neighbor — instead of stranding the
        // workspace on a deleted path.
        const stranded = $repoStore.openTabs.find((tab) => tab.path === wt.path);
        if (stranded) await repoStore.closeTab(stranded.id);
        return;
      }
      await load();
    } catch (err: unknown) {
      if ($repoStore.currentPath !== repo) return;
      harnessStore.recordAction({ kind: "worktree-remove", label: wt.path, ok: false });
      error = reportPanelError("worktrees", err);
    }
  }

  function open(wt: WorktreeInfo) {
    void repoStore.openRepo(wt.path);
  }
</script>

<div>
  <div class="flex items-center justify-between text-[10px] font-bold text-textMuted uppercase tracking-wider px-2 mb-1">
    <span class="flex items-center gap-1.5">
      <FolderGit2 size={11} />
      <span>Worktrees ({worktrees.length})</span>
    </span>
    <div class="flex items-center gap-1">
      <button
        onclick={prune}
        title="Prune stale worktree metadata"
        class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent transition-colors"
      >
        <Sparkles size={11} />
      </button>
      <button
        onclick={() => (showAddForm = !showAddForm)}
        title="Create a linked worktree for a parallel task"
        aria-label="Create worktree"
        class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent transition-colors"
      >
        <Plus size={12} />
      </button>
    </div>
  </div>

  {#if showAddForm}
    <form
      class="mx-1 mb-1 p-2.5 rounded-xl border border-border/70 bg-background space-y-1.5 shadow-card"
      onsubmit={(e) => {
        e.preventDefault();
        void create();
      }}
    >
      <input
        bind:value={newPath}
        placeholder="/absolute/path for the worktree"
        required
        class="w-full bg-surface border border-border/80 rounded-full px-2.5 py-1 font-mono text-[10px] text-textPrimary focus:outline-none focus:border-accent/60 transition-colors"
      />
      <div class="flex gap-1.5">
        <input
          bind:value={newBranch}
          placeholder="new branch (optional)"
          class="flex-1 min-w-0 bg-surface border border-border/80 rounded-full px-2.5 py-1 font-mono text-[10px] text-textPrimary focus:outline-none focus:border-accent/60 transition-colors"
        />
        <input
          bind:value={startPoint}
          placeholder="start point"
          class="flex-1 min-w-0 bg-surface border border-border/80 rounded-full px-2.5 py-1 font-mono text-[10px] text-textPrimary focus:outline-none focus:border-accent/60 transition-colors"
        />
      </div>
      <div class="flex items-center justify-between">
        <span class="text-[9px] text-textMuted">No branch name creates a detached checkout.</span>
        <button type="submit" disabled={isCreating || !newPath.trim()} class="gp-btn-primary !px-2 !py-0.5 !text-[10px]">
          {isCreating ? "Adding…" : "Add"}
        </button>
      </div>
    </form>
  {/if}

  {#if error}
    <div class="text-[10px] text-rose-400 px-2 py-1" title={error}>{error}</div>
  {/if}

  {#if isLoading && worktrees.length === 0}
    <div class="text-[11px] text-textMuted/60 px-2 py-1 italic">Loading…</div>
  {:else}
    <div class="space-y-0.5">
      {#each worktrees as wt (wt.path)}
        <div
          class="px-2 py-1 rounded-full flex items-center justify-between hover:bg-surfaceHover group transition-colors
            {wt.is_main ? '' : 'ring-1 ring-inset ring-accent/25'}"
        >
          <button
            class="flex items-center gap-1.5 min-w-0 flex-1 text-left"
            onclick={() => open(wt)}
            title="{wt.path}\n{wt.branch ?? 'detached'} · {wt.dirty_files === null ? 'not scanned' : wt.dirty_files + ' change(s)'}"
          >
            <span class="truncate text-[11px] font-medium text-textPrimary">{wt.name}</span>
            {#if wt.is_bare}
              <span class="shrink-0 text-[9px] uppercase rounded-full bg-surfaceHover border border-border/80 px-1 text-textMuted">bare</span>
            {/if}
            {#if wt.is_locked}
              <Lock size={10} class="shrink-0 text-textMuted" />
            {/if}
            {#if wt.is_prunable}
              <AlertTriangle size={10} class="shrink-0 text-amber-400" />
            {/if}
            <span class="shrink-0 text-[10px] font-mono text-textMuted truncate max-w-[90px]">
              {wt.branch ?? (wt.is_detached ? wt.head.slice(0, 7) : "")}
            </span>
            {#if (wt.dirty_files ?? 0) > 0}
              <span class="shrink-0 w-1.5 h-1.5 rounded-full bg-amber-400" title="{wt.dirty_files} uncommitted change(s)"></span>
            {/if}
          </button>
          <div class="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 shrink-0">
            <button onclick={() => open(wt)} title="Open in a new tab" class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent">
              <ExternalLink size={11} />
            </button>
            {#if !wt.is_main}
              <button
                onclick={() => void toggleLock(wt)}
                title={wt.is_locked ? "Unlock this worktree" : "Lock this worktree"}
                class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent"
              >
                {#if wt.is_locked}
                  <Unlock size={11} />
                {:else}
                  <Lock size={11} />
                {/if}
              </button>
              {#if removingPath === wt.path}
                <span class="shrink-0 text-[9px] font-semibold text-rose-400 whitespace-nowrap">
                  {(wt.dirty_files ?? 0) > 0 ? `Discard ${wt.dirty_files} changed files?` : "Remove?"}
                </span>
              {/if}
              <button
                onclick={() => void remove(wt)}
                title={removeArmTitle(wt)}
                aria-label={removeArmTitle(wt)}
                class="p-0.5 rounded-full {removingPath === wt.path ? 'text-rose-400' : 'hover:text-rose-400'}"
              >
                <Trash2 size={11} />
              </button>
            {/if}
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>
