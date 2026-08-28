<script lang="ts">
  import { onMount } from "svelte";
  import { repoStore } from "../../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import {
    Activity,
    RefreshCw,
    Check,
    Undo2,
    Layers,
    GitCommit,
    FileText,
    Clock,
    CheckCircle2,
    Flame,
  } from "lucide-svelte";
  import { formatAge } from "../../storage/format";
  import { askConfirm } from "../../stores/modalStore";
  import { promptQuickCommit } from "../../commit/quickCommit";
  import { createAsyncGuard, type AsyncGuard } from "../../async/guard";
  import { classifyFileChange, statusLiveKey, summarizeStatuses } from "../../files/fileStatus";

  interface FileCommit {
    id: string;
    author: string;
    date: number;
    message: string;
  }

  let {
    selectedFile = null,
    onSelectFile,
  }: {
    selectedFile?: string | null;
    onSelectFile?: (path: string) => void;
  } = $props();

  let isRefreshing = $state(false);
  let lastRefreshed = $state<number>(Date.now());
  let recentCommits = $state<FileCommit[]>([]);
  let isLoadingCommits = $state(false);
  let commitGuard: AsyncGuard | null = null;

  // Language info for selected file
  let detectedLang = $state<{ name: string; color_hex: string; category: string } | null>(null);

  let lastFingerprint = "";

  // Periodic refresh ticker for "just now" / relative age
  let tick = $state(0);
  onMount(() => {
    const timer = setInterval(() => (tick += 1), 5000);
    return () => {
      clearInterval(timer);
      commitGuard?.cancel();
    };
  });

  let nowMs = $derived.by(() => {
    void tick;
    return Date.now();
  });

  let statuses = $derived($repoStore.statuses);
  let dash = $derived(summarizeStatuses(statuses));

  let stagedFiles = $derived(statuses.filter((s) => classifyFileChange(s) === "staged"));
  let unstagedFiles = $derived(statuses.filter((s) => classifyFileChange(s) === "unstaged"));
  let untrackedFiles = $derived(statuses.filter((s) => classifyFileChange(s) === "untracked"));
  let conflictedFiles = $derived(statuses.filter((s) => classifyFileChange(s) === "conflict"));

  let totalAdditions = $derived(dash.additions);
  let totalDeletions = $derived(dash.deletions);
  let isDirty = $derived(dash.dirty > 0);

  let activeFileStatus = $derived.by(() => {
    if (!selectedFile) return null;
    return statuses.find((s) => s.path === selectedFile) || null;
  });

  async function handleRefresh() {
    isRefreshing = true;
    try {
      await repoStore.refresh();
      lastRefreshed = Date.now();
    } finally {
      isRefreshing = false;
    }
  }

  async function stageAll() {
    await repoStore.stageAll();
  }

  async function unstageAll() {
    await repoStore.unstageAll();
  }

  async function stageFile(path: string) {
    await repoStore.stageFile(path);
  }

  async function unstageFile(path: string) {
    await repoStore.unstageFile(path);
  }

  async function discardFile(path: string) {
    const confirmed = await askConfirm({
      title: "Discard Changes",
      message: `Discard all uncommitted changes to ${path}? This cannot be undone.`,
      confirmLabel: "Discard Changes",
    });
    if (!confirmed) return;
    await repoStore.discardChanges(path);
  }

  // Load commit history & language detection when selectedFile changes
  async function loadFileDetails(path: string) {
    const repo = $repoStore.currentPath;
    if (!repo || !path) return;

    commitGuard?.cancel();
    const guard = createAsyncGuard();
    commitGuard = guard;
    isLoadingCommits = true;

    try {
      // 1. Language detection
      try {
        const langInfo = await invoke<{ name: string; color_hex: string; category: string }>(
          "cmd_detect_language",
          { filePath: path }
        );
        if (guard.isLive()) detectedLang = langInfo;
      } catch {
        if (guard.isLive()) detectedLang = null;
      }

      // 2. Commit history touching this file
      const graph = await invoke<{ rows: Array<{ id: string; author_name: string; timestamp: number; summary: string }> }>(
        "cmd_get_commit_graph",
        {
          repoPath: repo,
          maxCommits: 8,
          query: `path:${path}`,
          revision: null,
          skip: 0,
        }
      );

      if (!guard.isLive()) return;
      recentCommits = (graph.rows || []).slice(0, 6).map((row) => ({
        id: row.id,
        author: row.author_name,
        date: row.timestamp * 1000,
        message: row.summary,
      }));
    } catch {
      if (guard.isLive()) recentCommits = [];
    } finally {
      if (guard.isLive()) isLoadingCommits = false;
    }
  }

  $effect(() => {
    const key = statusLiveKey($repoStore.statuses);
    if (key === lastFingerprint) return;
    lastFingerprint = key;
    lastRefreshed = Date.now();
  });

  $effect(() => {
    if (selectedFile) {
      void loadFileDetails(selectedFile);
    } else {
      recentCommits = [];
      detectedLang = null;
    }
  });
</script>

<div class="flex flex-col h-full bg-surface/40 font-sans text-xs min-h-0 border-l border-border/70 select-none overflow-y-auto gp-scroll">
  <!-- Top Live Pulse Header -->
  <div class="flex items-center justify-between px-3 py-2.5 border-b border-border/60 bg-surface/80 shrink-0">
    <div class="flex items-center gap-2">
      {#if isDirty}
        <span class="relative flex h-2.5 w-2.5">
          <span class="animate-ping absolute inline-flex h-full w-full rounded-full bg-amber-400 opacity-75"></span>
          <span class="relative inline-flex rounded-full h-2.5 w-2.5 bg-amber-500"></span>
        </span>
      {:else}
        <span class="relative inline-flex rounded-full h-2.5 w-2.5 bg-emerald-500"></span>
      {/if}
      <span class="text-[11px] font-bold uppercase tracking-wider text-textPrimary">Live Pulse</span>
      <span class="text-[10px] text-textMuted font-mono">({formatAge(lastRefreshed, nowMs)})</span>
    </div>

    <div class="flex items-center gap-1">
      <button
        type="button"
        onclick={handleRefresh}
        title="Sync Status"
        class="gp-icon-btn !p-1 text-textMuted hover:text-textPrimary"
      >
        <RefreshCw size={12} class={isRefreshing || $repoStore.isLoading ? "animate-spin" : ""} />
      </button>
    </div>
  </div>

  <div class="p-3 space-y-4">
    <!-- Uncommitted Churn Metrics Grid -->
    <div>
      <div class="flex items-center justify-between text-[11px] font-semibold text-textMuted mb-2 uppercase tracking-wide">
        <div class="flex items-center gap-1.5">
          <Activity size={12} class="text-accent" />
          <span>Uncommitted Status</span>
        </div>
        {#if totalAdditions > 0 || totalDeletions > 0}
          <div class="flex items-center gap-1.5 font-mono text-[10px]">
            <span class="text-emerald-400 font-bold">+{totalAdditions}</span>
            <span class="text-rose-400 font-bold">-{totalDeletions}</span>
          </div>
        {/if}
      </div>

      <div class="grid grid-cols-2 gap-2">
        <div class="p-2.5 rounded-xl border border-border/70 bg-surface/60 shadow-sm flex flex-col">
          <span class="text-[10px] text-textMuted">Staged</span>
          <span class="text-base font-bold text-emerald-400 tabular-nums">{stagedFiles.length}</span>
        </div>
        <div class="p-2.5 rounded-xl border border-border/70 bg-surface/60 shadow-sm flex flex-col">
          <span class="text-[10px] text-textMuted">Modified</span>
          <span class="text-base font-bold text-amber-400 tabular-nums">{unstagedFiles.length}</span>
        </div>
        <div class="p-2.5 rounded-xl border border-border/70 bg-surface/60 shadow-sm flex flex-col">
          <span class="text-[10px] text-textMuted">Untracked</span>
          <span class="text-base font-bold text-cyan-400 tabular-nums">{untrackedFiles.length}</span>
        </div>
        <div class="p-2.5 rounded-xl border border-border/70 bg-surface/60 shadow-sm flex flex-col">
          <span class="text-[10px] text-textMuted">Conflicted</span>
          <span class="text-base font-bold {conflictedFiles.length > 0 ? 'text-rose-400' : 'text-textMuted'} tabular-nums">{conflictedFiles.length}</span>
        </div>
      </div>

      {#if statuses.length > 0}
        <div class="flex items-center gap-2 mt-2.5">
          {#if unstagedFiles.length > 0 || untrackedFiles.length > 0}
            <button
              type="button"
              onclick={stageAll}
              class="gp-btn flex-1 !py-1 !text-[11px] justify-center text-emerald-300 font-semibold"
            >
              <Check size={11} />
              <span>Stage All</span>
            </button>
          {/if}
          {#if stagedFiles.length > 0}
            <button
              type="button"
              onclick={unstageAll}
              class="gp-btn flex-1 !py-1 !text-[11px] justify-center text-amber-300 font-semibold"
            >
              <Undo2 size={11} />
              <span>Unstage All</span>
            </button>
            <button
              type="button"
              onclick={() => void promptQuickCommit()}
              class="gp-btn-primary flex-1 !py-1 !text-[11px] justify-center"
            >
              <GitCommit size={11} />
              <span>Commit</span>
            </button>
          {/if}
        </div>
      {/if}
    </div>

    <!-- Live Uncommitted Modifications Stream -->
    <div>
      <div class="flex items-center justify-between text-[11px] font-semibold text-textMuted mb-2 uppercase tracking-wide">
        <div class="flex items-center gap-1.5">
          <Flame size={12} class="text-amber-400" />
          <span>Active Changes ({statuses.length})</span>
        </div>
      </div>

      {#if statuses.length === 0}
        <div class="p-4 rounded-xl border border-border/60 bg-surface/30 text-center text-textMuted text-xs flex flex-col items-center gap-1.5">
          <CheckCircle2 size={16} class="text-emerald-400" />
          <span class="font-medium text-textPrimary">Working Tree Clean</span>
          <span class="text-[10px] text-textMuted">No pending or uncommitted modifications</span>
        </div>
      {:else}
        <div class="space-y-1.5 max-h-64 overflow-y-auto gp-scroll pr-0.5">
          {#each statuses as s}
            {@const isSelected = selectedFile === s.path}
            {@const kind = classifyFileChange(s)}
            <div
              class="p-2 rounded-xl border transition-all flex items-center justify-between gap-2 {isSelected
                ? 'border-accent/60 bg-accent/10 shadow-sm'
                : 'border-border/60 bg-surface/50 hover:border-border'}"
            >
              <button
                type="button"
                onclick={() => onSelectFile?.(s.path)}
                class="flex-1 min-w-0 text-left cursor-pointer"
              >
                <div class="flex items-center gap-1.5 min-w-0">
                  {#if kind === "conflict"}
                    <span class="px-1 py-0.2 text-[9px] font-bold rounded bg-rose-500/20 text-rose-300 border border-rose-500/40">!C</span>
                  {:else if kind === "staged"}
                    <span class="px-1 py-0.2 text-[9px] font-bold rounded bg-emerald-500/20 text-emerald-300 border border-emerald-500/40">S</span>
                  {:else if kind === "untracked"}
                    <span class="px-1 py-0.2 text-[9px] font-bold rounded bg-cyan-500/20 text-cyan-300 border border-cyan-500/40">U</span>
                  {:else}
                    <span class="px-1 py-0.2 text-[9px] font-bold rounded bg-amber-500/20 text-amber-300 border border-amber-500/40">M</span>
                  {/if}
                  <span class="text-xs font-medium text-textPrimary truncate">{s.path.slice(s.path.lastIndexOf("/") + 1)}</span>
                </div>
                <div class="text-[10px] text-textMuted font-mono truncate max-w-[180px] pl-4">
                  {s.path}
                </div>
              </button>

              <div class="flex items-center gap-1 shrink-0">
                {#if s.additions > 0 || s.deletions > 0}
                  <span class="text-[9px] font-mono text-emerald-400">+{s.additions}</span>
                  <span class="text-[9px] font-mono text-rose-400">-{s.deletions}</span>
                {/if}

                {#if s.is_staged}
                  <button
                    type="button"
                    onclick={() => unstageFile(s.path)}
                    title="Unstage"
                    class="p-1 rounded hover:bg-surface text-textMuted hover:text-amber-300"
                  >
                    <Undo2 size={12} />
                  </button>
                {:else}
                  <button
                    type="button"
                    onclick={() => stageFile(s.path)}
                    title="Stage"
                    class="p-1 rounded hover:bg-surface text-textMuted hover:text-emerald-300"
                  >
                    <Check size={12} />
                  </button>
                  <button
                    type="button"
                    onclick={() => discardFile(s.path)}
                    title="Discard changes"
                    class="p-1 rounded hover:bg-surface text-textMuted hover:text-rose-400"
                  >
                    <Undo2 size={12} />
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </div>

    <!-- Active File Inspector -->
    {#if selectedFile}
      <div class="pt-2 border-t border-border/60">
        <div class="flex items-center justify-between text-[11px] font-semibold text-textMuted mb-2 uppercase tracking-wide">
          <div class="flex items-center gap-1.5">
            <FileText size={12} class="text-purple-400" />
            <span>File Inspector</span>
          </div>
          {#if detectedLang}
            <span
              class="px-1.5 py-0.5 text-[9px] font-mono font-bold rounded"
              style="background-color: {detectedLang.color_hex}25; color: {detectedLang.color_hex};"
            >
              {detectedLang.name}
            </span>
          {/if}
        </div>

        <div class="p-3 rounded-xl border border-border/70 bg-surface/50 space-y-2.5">
          <div class="flex items-center justify-between">
            <span class="text-[10px] text-textMuted font-mono truncate max-w-[200px]" title={selectedFile}>{selectedFile}</span>
            {#if activeFileStatus}
              {@const kind = classifyFileChange(activeFileStatus)}
              <span class="px-1.5 py-0.5 text-[9px] font-bold rounded {kind === 'conflict'
                ? 'bg-rose-500/20 text-rose-300'
                : kind === 'staged'
                  ? 'bg-emerald-500/20 text-emerald-300'
                  : kind === 'untracked'
                    ? 'bg-cyan-500/20 text-cyan-300'
                    : 'bg-amber-500/20 text-amber-300'}">
                {kind === "conflict" ? "CONFLICT" : kind === "staged" ? "STAGED" : kind === "untracked" ? "UNTRACKED" : "MODIFIED"}
              </span>
            {:else}
              <span class="px-1.5 py-0.5 text-[9px] font-bold rounded bg-emerald-500/15 text-emerald-400">
                Clean (HEAD)
              </span>
            {/if}
          </div>

          <div class="flex items-center gap-2 pt-1 border-t border-border/50">
            <button
              type="button"
              onclick={() => { repoStore.selectFilePath(selectedFile); repoStore.setActiveTab('diff'); }}
              class="gp-btn flex-1 !py-1 !text-[10px] justify-center"
            >
              <Layers size={11} class="text-cyan-400" />
              <span>Diff</span>
            </button>
            <button
              type="button"
              onclick={() => { repoStore.selectFilePath(selectedFile); repoStore.setActiveTab('blame'); }}
              class="gp-btn flex-1 !py-1 !text-[10px] justify-center"
            >
              <GitCommit size={11} class="text-purple-400" />
              <span>Blame</span>
            </button>
          </div>
        </div>

        <!-- Recent Commits Touching This File -->
        <div class="mt-3">
          <div class="flex items-center gap-1.5 text-[11px] font-semibold text-textMuted mb-2 uppercase tracking-wide">
            <Clock size={12} />
            <span>Recent File History</span>
          </div>

          {#if isLoadingCommits}
            <div class="p-3 text-center text-textMuted text-xs">Loading file history...</div>
          {:else if recentCommits.length === 0}
            <div class="p-3 rounded-xl border border-border/60 bg-surface/30 text-center text-textMuted text-[11px]">
              No prior commits recorded for this path
            </div>
          {:else}
            <div class="space-y-1.5">
              {#each recentCommits as c}
                <button
                  type="button"
                  onclick={() => repoStore.inspectCommitInHistory(c.id)}
                  class="w-full text-left p-2 rounded-lg border border-border/60 bg-surface/40 hover:bg-surface/70 transition-colors"
                  title="Inspect {c.id.slice(0, 7)} on the graph"
                >
                  <div class="flex items-center justify-between text-[10px]">
                    <span class="font-mono text-accent font-semibold">{c.id.slice(0, 7)}</span>
                    <span class="text-textMuted font-mono">{formatAge(c.date, nowMs)}</span>
                  </div>
                  <div class="text-xs text-textPrimary font-medium truncate mt-0.5" title={c.message}>
                    {c.message}
                  </div>
                  <div class="text-[10px] text-textMuted truncate mt-0.5">
                    by {c.author}
                  </div>
                </button>
              {/each}
            </div>
          {/if}
        </div>
      </div>
    {/if}
  </div>
</div>
