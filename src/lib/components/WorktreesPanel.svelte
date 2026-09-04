<script lang="ts">
  import type { WorktreeInfo } from "../branches/types";
  import type { TaskScope, TaskView } from "../tasks/types";
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
  import { agentKind, agentSessionSlug, isAgentWorktree } from "../work/agentWorktree";


  let worktrees = $state<WorktreeInfo[]>([]);
  /**
   * DevCouncil state for this repository, and which worktree is bound to what.
   *
   * A worktree stops being a directory and becomes a task in flight once it is
   * bound: every mutation inside it is then judged against that task's planned
   * files rather than against no plan at all.
   */
  let taskView = $state<TaskView | null>(null);
  let bindings = $state<Record<string, string | null>>({});
  /** Declared scope per bound task, for the planned-file summary. */
  let scopes = $state<Record<string, TaskScope>>({});
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
      await loadTaskState(repo, next, guard);
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      error = reportPanelError("worktrees", err);
    } finally {
      if (guard.isLive()) isLoading = false;
    }
  }

  /**
   * Loads DevCouncil leases and each worktree's binding.
   *
   * Deliberately non-fatal: most repositories have no DevCouncil store, and
   * that is not an error. A failure here leaves the worktree list intact and
   * simply shows no task column — the panel's own job still works.
   */
  async function loadTaskState(repo: string, list: WorktreeInfo[], guard: AsyncGuard) {
    try {
      const view = await invoke<TaskView>("cmd_task_view", { repoPath: repo });
      if (!guard.isLive()) return;
      taskView = view;
      const resolved: Record<string, string | null> = {};
      for (const wt of list) {
        resolved[wt.path] = await invoke<string | null>("cmd_worktree_task", {
          repoPath: repo,
          worktreePath: wt.path,
        });
        if (!guard.isLive()) return;
      }
      bindings = resolved;

      // The declared scope of each bound task, so a row can say how much this
      // worktree is authorised to touch. Fetched only for tasks actually bound
      // here — the store may hold hundreds.
      const wanted = [...new Set(Object.values(resolved).filter((t): t is string => !!t))];
      const loaded: Record<string, TaskScope> = {};
      for (const taskId of wanted) {
        const scope = await invoke<TaskScope | null>("cmd_task_scope", {
          repoPath: repo,
          taskId,
        });
        if (!guard.isLive()) return;
        if (scope) loaded[taskId] = scope;
      }
      scopes = loaded;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      // Reported to diagnostics, not to the panel's error banner: task state is
      // an enrichment, and losing it must not read as "the worktrees failed".
      reportPanelError("worktrees", err);
      taskView = null;
    }
  }

  /** Active leases keyed by task, for the lease badge. */
  const leaseFor = $derived((taskId: string | null) =>
    taskId ? (taskView?.leases.find((l) => l.task_id === taskId) ?? null) : null,
  );

  async function bindTask(wt: WorktreeInfo, taskId: string) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    try {
      await invoke<number>("cmd_bind_worktree_task", {
        repoPath: repo,
        worktreePath: wt.path,
        taskId,
      });
      bindings = { ...bindings, [wt.path]: taskId };
    } catch (err: unknown) {
      error = reportPanelError("worktrees", err);
    }
  }

  async function unbindTask(wt: WorktreeInfo) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    try {
      await invoke<number>("cmd_unbind_worktree_task", {
        repoPath: repo,
        worktreePath: wt.path,
      });
      bindings = { ...bindings, [wt.path]: null };
    } catch (err: unknown) {
      error = reportPanelError("worktrees", err);
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
    const targetPath = newPath.trim();
    const branch = newBranch.trim();
    const base = startPoint.trim();
    if (!repo || !targetPath) return;
    const actionLabel = branch ? `${branch} → ${targetPath}` : targetPath;
    let createCompleted = false;
    isCreating = true;
    error = null;
    try {
      await invoke("cmd_add_worktree", {
        repoPath: repo,
        targetPath,
        newBranch: branch || null,
        startPoint: base || null,
        detach: !branch,
      });
      createCompleted = true;
      harnessStore.recordAction({
        repoPath: repo,
        kind: "worktree",
        label: actionLabel,
        ok: true,
      });
      if ($repoStore.currentPath !== repo) return;
      newPath = "";
      newBranch = "";
      startPoint = "";
      showAddForm = false;
      await load();
    } catch (err: unknown) {
      if (!createCompleted) {
        harnessStore.recordAction({ repoPath: repo, kind: "worktree", label: actionLabel, ok: false });
      }
      if ($repoStore.currentPath !== repo) return;
      error = reportPanelError("worktrees", err);
    } finally {
      isCreating = false;
    }
  }

  /** Armed-confirm label; names the discard cost when changes would be lost. */
  function removeArmTitle(wt: WorktreeInfo): string {
    if (removingPath !== wt.path) return "Remove this worktree";
    if (wt.dirty_files === null || wt.dirty_files === undefined) {
      return "Not scanned for changes — click again to try a clean remove";
    }
    return wt.dirty_files > 0
      ? `Discard ${wt.dirty_files} changed files? Click again to remove`
      : "Click again to remove";
  }

  async function remove(wt: WorktreeInfo) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const targetPath = wt.path;
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
    // --force destroys uncommitted work. An unscanned worktree (null) is
    // not a verified-clean one: never force-remove it. A scanned dirty
    // tree is force-removed only after the armed confirm named the cost.
    const force = typeof wt.dirty_files === "number" && wt.dirty_files > 0;
    let removeCompleted = false;
    try {
      await invoke("cmd_remove_worktree", { repoPath: repo, targetPath, force });
      removeCompleted = true;
      harnessStore.recordAction({ repoPath: repo, kind: "worktree-remove", label: targetPath, ok: true });
      if ($repoStore.currentPath !== repo) return;
      if ($repoStore.currentPath === targetPath) {
        // T-F09: the active tab points INTO the removed directory. Close it —
        // which activates a surviving neighbor — instead of stranding the
        // workspace on a deleted path.
        const stranded = $repoStore.openTabs.find((tab) => tab.path === targetPath);
        if (stranded) await repoStore.closeTab(stranded.id);
        return;
      }
      await load();
    } catch (err: unknown) {
      if (!removeCompleted) {
        harnessStore.recordAction({ repoPath: repo, kind: "worktree-remove", label: targetPath, ok: false });
      }
      if ($repoStore.currentPath !== repo) return;
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
        aria-label="Prune stale worktree metadata"
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
          class="px-2 py-1 rounded-2xl flex flex-col hover:bg-surfaceHover group transition-colors
            {wt.is_main ? '' : 'ring-1 ring-inset ring-accent/25'}"
        >
         <div class="flex items-center justify-between">
          <button
            class="flex items-center gap-1.5 min-w-0 flex-1 text-left"
            onclick={() => open(wt)}
            title="{wt.path}\n{wt.branch ?? 'detached'} · {wt.dirty_files === null ? 'not scanned' : wt.dirty_files + ' change(s)'}"
          >
            <span class="truncate text-[11px] font-medium text-textPrimary">{wt.name}</span>
            {#if isAgentWorktree(wt.path)}
              <span
                class="shrink-0 text-[9px] uppercase rounded-full bg-accent/10 px-1 text-accent"
                title="Agent session {agentSessionSlug(wt.path) || agentKind(wt.path)}"
              >{agentKind(wt.path)}</span>
            {/if}
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
            <button
              onclick={() => open(wt)}
              title="Open in a new tab"
              aria-label="Open {wt.name} in a new tab"
              class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent"
            >
              <ExternalLink size={11} />
            </button>
            {#if !wt.is_main}
              <button
                onclick={() => void toggleLock(wt)}
                title={wt.is_locked ? "Unlock this worktree" : "Lock this worktree"}
                aria-label={wt.is_locked ? `Unlock ${wt.name}` : `Lock ${wt.name}`}
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
                  {typeof wt.dirty_files === "number" && wt.dirty_files > 0
                  ? `Discard ${wt.dirty_files} changed files?`
                  : wt.dirty_files === null
                    ? "Not scanned — clean remove?"
                    : "Remove?"}
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

          <!--
            The task line. A worktree bound to a DevCouncil task has every
            mutation inside it judged against that task's planned files; an
            unbound one is judged against no plan at all, which the harness
            reports as task.absent. Showing which is which is the point: the
            two are different safety postures and used to look identical.
          -->
          {#if taskView?.available}
            {@const bound = bindings[wt.path] ?? null}
            {@const lease = leaseFor(bound)}
            <div class="flex items-center gap-1 pl-1 pb-0.5 min-w-0">
              {#if bound}
                <span
                  class="shrink-0 text-[9px] font-mono rounded-full bg-accent/15 text-accent border border-accent/30 px-1.5"
                  title="Mutations in this worktree are judged against {bound}'s planned files"
                >{bound}</span>
                {#if scopes[bound]}
                  {@const sc = scopes[bound]}
                  <span
                    class="shrink-0 text-[9px] text-textMuted"
                    title="Planned: {sc.planned_files.join(', ') || 'nothing'}{sc.agent_appended_files
                      .length
                      ? `\nAgent-appended: ${sc.agent_appended_files.join(', ')}`
                      : ''}{sc.forbidden_changes.length
                      ? `\nForbidden: ${sc.forbidden_changes.join(', ')}`
                      : ''}"
                  >{sc.planned_files.length} planned{sc.agent_appended_files.length
                    ? ` +${sc.agent_appended_files.length} widened`
                    : ""}</span>
                {/if}
                {#if lease}
                  <span
                    class="shrink-0 text-[9px] text-textMuted truncate"
                    title="Leased by {lease.owner}{lease.agent ? ` (${lease.agent})` : ''}{lease.expires_at
                      ? `, expires ${lease.expires_at}`
                      : ', no expiry'}"
                  >{lease.agent ?? lease.owner}{lease.expires_at ? "" : " · no expiry"}</span>
                {:else}
                  <!--
                    Bound but unleased is a real state, not a missing one: the
                    binding says what this checkout is for, the lease says who
                    currently holds it, and a task can be bound here while
                    nobody holds it.
                  -->
                  <span class="shrink-0 text-[9px] text-textMuted">not leased</span>
                {/if}
                <button
                  onclick={() => void unbindTask(wt)}
                  title="Clear this worktree's task binding"
                  class="ml-auto shrink-0 text-[9px] text-textMuted hover:text-accent opacity-0 group-hover:opacity-100"
                >unbind</button>
              {:else if taskView.leases.length > 0}
                <select
                  class="text-[9px] bg-transparent text-textMuted border border-border/60 rounded-full px-1 py-px hover:text-accent focus:outline-none focus:border-accent/60"
                  aria-label="Bind {wt.name} to a task"
                  onchange={(e) => {
                    const id = (e.currentTarget as HTMLSelectElement).value;
                    if (id) void bindTask(wt, id);
                  }}
                >
                  <option value="">no task</option>
                  {#each taskView.leases as l (l.task_id)}
                    <option value={l.task_id}>{l.task_id}</option>
                  {/each}
                </select>
              {/if}
            </div>
          {:else if taskView && taskView.error}
            <!--
              A store we could not read is not a repository without one. Saying
              so is the difference between "no tasks here" and "we do not know".
            -->
            <div class="pl-1 pb-0.5 text-[9px] text-amber-400 truncate" title={taskView.error}>
              task state unavailable
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>
