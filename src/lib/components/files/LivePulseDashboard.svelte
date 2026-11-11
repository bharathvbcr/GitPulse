<script lang="ts">
  import type { LanguageInfo } from "../../files/types";
  import type { CommitGraphPayload } from "../../stores/graphStore";
  import { onMount, untrack } from "svelte";
  import { createAdaptiveTimer } from "../../runtime/adaptiveTimer";
  import { currentRepoPath, repoStore } from "../../stores/repoStore";
  import { currentHeadTip } from "../../stores/headTip";
  import { invoke } from "../../ipc/invoke";
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
  } from "@lucide/svelte";
  import { formatAge } from "../../storage/format";
  import { askConfirm } from "../../stores/modalStore";
  import { reportFailure } from "../../stores/mutationOutcome";
  import { promptQuickCommit } from "../../commit/quickCommit";
  import { createAsyncGuard, type AsyncGuard } from "../../async/guard";
  import {
    classifyFileChange,
    statusBadgeClass,
    statusBadgeLabel,
    statusLiveKey,
    summarizeStatuses,
  } from "../../files/fileStatus";
  import { formatPathParts } from "../../files/formatPath";
  import LanguageLogo from "../LanguageLogo.svelte";
  import Skeleton from "../Skeleton.svelte";

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

  // Periodic refresh ticker for "just now" / relative age
  let tick = $state(0);
  onMount(() => {
    // Relative-age labels only need re-rendering while someone is looking at
    // them. The status poll already refuses to work on a hidden window; this
    // ticker used to keep firing behind other windows, invalidating derived
    // state across the pane every five seconds for a label nobody could see.
    const stopTicking = createAdaptiveTimer(() => (tick += 1), 5000);
    return () => {
      stopTicking();
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
    return await repoStore.stageAll();
  }

  async function unstageAll() {
    return await repoStore.unstageAll();
  }

  async function stageFile(path: string) {
    return await repoStore.stageFile(path);
  }

  async function unstageFile(path: string) {
    return await repoStore.unstageFile(path);
  }

  async function discardFile(path: string) {
    // The repository this file belongs to, held across the confirm.
    const target = repoStore.captureTarget();
    const confirmed = await askConfirm({
      title: "Discard changes",
      message: `Discard all uncommitted changes to ${path}? This cannot be undone.`,
      confirmLabel: "Discard changes",
    });
    if (!confirmed) return;
    const outcome = await repoStore.discardChanges(path, target);
    reportFailure(outcome, "Could not discard the changes");
  }

  // Load commit history & language detection when selectedFile changes
  async function loadFileDetails(repo: string, path: string) {
    commitGuard?.cancel();
    const guard = createAsyncGuard();
    commitGuard = guard;
    isLoadingCommits = true;
    // The heading changes to the new path synchronously. Clear the previous
    // path's payload in the same turn so its language/history can never be
    // presented under the new identity while either command is pending.
    detectedLang = null;
    recentCommits = [];

    try {
      // 1. Language detection
      try {
        const langInfo = await invoke<LanguageInfo>(
          "cmd_detect_language",
          { filePath: path }
        );
        if (guard.isLive()) detectedLang = langInfo;
      } catch {
        if (guard.isLive()) detectedLang = null;
      }

      // 2. Commit history touching this file
      const graph = await invoke<CommitGraphPayload>(
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

  const statusKey = $derived(statusLiveKey($repoStore.statuses));
  $effect(() => {
    void statusKey;
    untrack(() => (lastRefreshed = Date.now()));
  });

  // Keyed by value on the repository, the selection and HEAD (see
  // `currentHeadTip`): a commit, pull or checkout is what can change this
  // file's history, and the store's other publishes are not. No teardown
  // cancels the load: `loadFileDetails` cancels its own predecessor, and
  // unmount is handled in `onMount`.
  $effect(() => {
    const repo = $currentRepoPath;
    const file = selectedFile;
    void $currentHeadTip;
    untrack(() => {
      if (repo && file) {
        void loadFileDetails(repo, file);
      } else {
        commitGuard?.cancel();
        commitGuard = null;
        isLoadingCommits = false;
        recentCommits = [];
        detectedLang = null;
      }
    });
  });
</script>

<div class="flex flex-col h-full bg-surface/40 font-sans text-xs min-h-0 select-none overflow-y-auto gp-scroll">
  <!-- Top Live Pulse Header -->
  <div class="flex items-center justify-between px-3 py-2.5 gp-section-edge bg-surface/80 shrink-0">
    <div class="flex items-center gap-2">
      {#if isDirty}
        <span class="relative flex h-2.5 w-2.5">
          <span class="animate-ping absolute inline-flex h-full w-full rounded-full bg-warn opacity-75"></span>
          <span class="relative inline-flex rounded-full h-2.5 w-2.5 bg-warn"></span>
        </span>
      {:else}
        <span class="relative inline-flex rounded-full h-2.5 w-2.5 bg-ok"></span>
      {/if}
      <span class="text-meta font-semibold text-textPrimary">Live pulse</span>
      <span class="text-micro text-textMuted font-mono">({formatAge(lastRefreshed, nowMs)})</span>
    </div>

    <div class="flex items-center gap-1">
      <button
        type="button"
        onclick={handleRefresh}
        title="Refresh status"
        aria-label="Refresh status"
        class="gp-icon-btn"
        data-size="sm"
      >
        <RefreshCw size={12} class={isRefreshing || $repoStore.isLoading ? "animate-spin" : ""} />
      </button>
    </div>
  </div>

  <div class="p-3 space-y-4">
    <!-- Uncommitted Churn Metrics Grid -->
    <div>
      <div class="flex items-center justify-between gp-label mb-2">
        <div class="flex items-center gap-1.5">
          <Activity size={12} class="text-accent" />
          <span>Uncommitted status</span>
        </div>
        {#if totalAdditions > 0 || totalDeletions > 0}
          <div class="flex items-center gap-1.5 font-mono text-micro">
            <span class="text-ok font-semibold">+{totalAdditions}</span>
            <span class="text-danger font-semibold">-{totalDeletions}</span>
          </div>
        {/if}
      </div>

      <div class="grid grid-cols-2 gap-2">
        <div class="p-2.5 rounded-xl border border-edgeSoft bg-surface/60 shadow-xs flex flex-col">
          <span class="text-micro text-textMuted">Staged</span>
          <span class="text-base font-semibold text-ok tabular-nums">{stagedFiles.length}</span>
        </div>
        <div class="p-2.5 rounded-xl border border-edgeSoft bg-surface/60 shadow-xs flex flex-col">
          <span class="text-micro text-textMuted">Modified</span>
          <span class="text-base font-semibold text-warn tabular-nums">{unstagedFiles.length}</span>
        </div>
        <div class="p-2.5 rounded-xl border border-edgeSoft bg-surface/60 shadow-xs flex flex-col">
          <span class="text-micro text-textMuted">Untracked</span>
          <span class="text-base font-semibold text-info tabular-nums">{untrackedFiles.length}</span>
        </div>
        <div class="p-2.5 rounded-xl border border-edgeSoft bg-surface/60 shadow-xs flex flex-col">
          <span class="text-micro text-textMuted">Conflicted</span>
          <span class="text-base font-semibold {conflictedFiles.length > 0 ? 'text-danger' : 'text-textMuted'} tabular-nums">{conflictedFiles.length}</span>
        </div>
      </div>

      {#if statuses.length > 0}
        <div class="flex items-center gap-2 mt-2.5">
          {#if unstagedFiles.length > 0 || untrackedFiles.length > 0}
            <button
              type="button"
              onclick={stageAll}
              class="gp-btn flex-1 justify-center text-ok" data-size="sm"
            >
              <Check size={11} />
              <span>Stage all</span>
            </button>
          {/if}
          {#if stagedFiles.length > 0}
            <button
              type="button"
              onclick={unstageAll}
              class="gp-btn flex-1 justify-center text-warn" data-size="sm"
            >
              <Undo2 size={11} />
              <span>Unstage all</span>
            </button>
            <button
              type="button"
              onclick={() => void promptQuickCommit()}
              class="gp-btn flex-1 justify-center" data-variant="primary" data-size="sm"
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
      <div class="flex items-center justify-between gp-label mb-2">
        <div class="flex items-center gap-1.5">
          <Flame size={12} class="text-warn" />
          <span>Active changes ({statuses.length})</span>
        </div>
      </div>

      {#if statuses.length === 0}
        <div class="p-4 rounded-xl border border-edgeSoft bg-surface/30 text-center text-textMuted text-xs flex flex-col items-center gap-1.5">
          <CheckCircle2 size={16} class="text-ok" />
          <span class="font-medium text-textPrimary">Working tree clean</span>
          <span class="text-micro text-textMuted">No pending or uncommitted modifications</span>
        </div>
      {:else}
        <div class="space-y-1.5 pr-0.5">
          {#each statuses as s}
            {@const isSelected = selectedFile === s.path}
            {@const kind = classifyFileChange(s)}
            <div
              class="p-2 rounded-xl border transition-colors flex items-center justify-between gap-2 {isSelected
                ? 'border-accent bg-accent/10 shadow-xs'
                : 'border-edgeSoft bg-surface/50 hover:border-border'}"
            >
              <button
                type="button"
                onclick={() => onSelectFile?.(s.path)}
                class="flex-1 min-w-0 text-left cursor-pointer"
              >
                <div class="flex items-center gap-1.5 min-w-0">
                  <LanguageLogo filePath={s.path} size={12} class="shrink-0" />
                  <span class="px-1 py-0.2 text-micro font-semibold rounded {statusBadgeClass(kind)}">{statusBadgeLabel(kind)}</span>
                  <span class="min-w-0 text-xs font-medium text-textPrimary truncate">{formatPathParts(s.path).name}</span>
                </div>
                <div class="text-micro text-textMuted font-mono truncate max-w-[180px] pl-4">
                  {s.path}
                </div>
              </button>

              <div class="flex items-center gap-1 shrink-0">
                {#if s.additions > 0 || s.deletions > 0}
                  <span class="text-micro font-mono text-ok">+{s.additions}</span>
                  <span class="text-micro font-mono text-danger">-{s.deletions}</span>
                {/if}

                {#if s.is_staged}
                  <button
                    type="button"
                    onclick={() => unstageFile(s.path)}
                    title="Unstage"
                    aria-label="Unstage"
                    class="gp-icon-btn hover:text-warn"
                    data-size="sm"
                  >
                    <Undo2 size={12} />
                  </button>
                {:else}
                  <button
                    type="button"
                    onclick={() => stageFile(s.path)}
                    title="Stage"
                    aria-label="Stage"
                    class="gp-icon-btn hover:text-ok"
                    data-size="sm"
                  >
                    <Check size={12} />
                  </button>
                  <button
                    type="button"
                    onclick={() => discardFile(s.path)}
                    title="Discard changes"
                    aria-label="Discard changes"
                    class="gp-icon-btn hover:text-danger"
                    data-size="sm"
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
      <div class="pt-3">
        <div class="flex items-center justify-between gp-label mb-2">
          <div class="flex items-center gap-1.5">
            <FileText size={12} class="text-accent" />
            <span>File inspector</span>
          </div>
          {#if detectedLang}
            <span
              class="px-1.5 py-0.5 text-micro font-mono font-semibold rounded flex items-center gap-1"
              style="background-color: {detectedLang.color_hex}25; color: {detectedLang.color_hex};"
            >
              <LanguageLogo language={detectedLang.name} size={11} class="shrink-0" />
              <span>{detectedLang.name}</span>
            </span>
          {/if}
        </div>

        <div class="p-3 rounded-xl border border-edgeSoft bg-surface/50 space-y-2.5">
          <div class="flex items-center justify-between">
            <span class="text-micro text-textMuted font-mono truncate max-w-[200px]" title={selectedFile}>{selectedFile}</span>
            {#if activeFileStatus}
              {@const kind = classifyFileChange(activeFileStatus)}
              <span class="px-1.5 py-0.5 text-micro font-semibold rounded {kind === 'conflict'
                ? 'bg-danger/15 text-danger'
                : kind === 'staged'
                  ? 'bg-ok/15 text-ok'
                  : kind === 'untracked'
                    ? 'bg-info/15 text-info'
                    : 'bg-warn/15 text-warn'}">
                {kind === "conflict" ? "Conflict" : kind === "staged" ? "Staged" : kind === "untracked" ? "Untracked" : "Modified"}
              </span>
            {:else}
              <span class="px-1.5 py-0.5 text-micro font-semibold rounded bg-ok/15 text-ok">
                Clean (HEAD)
              </span>
            {/if}
          </div>

          <div class="flex items-center gap-2 pt-2">
            <button
              type="button"
              onclick={() => { repoStore.selectFilePath(selectedFile); repoStore.setActiveTab('history', 'diff'); }}
              class="gp-btn flex-1 justify-center" data-size="sm"
            >
              <Layers size={11} class="text-info" />
              <span>Diff</span>
            </button>
            <button
              type="button"
              onclick={() => { repoStore.selectFilePath(selectedFile); repoStore.setActiveTab('code', 'blame'); }}
              class="gp-btn flex-1 justify-center" data-size="sm"
            >
              <GitCommit size={11} class="text-accent" />
              <span>Blame</span>
            </button>
          </div>
        </div>

        <!-- Recent Commits Touching This File -->
        <div class="mt-3">
          <div class="flex items-center gap-1.5 gp-label mb-2">
            <Clock size={12} />
            <span>Recent file history</span>
          </div>

          {#if isLoadingCommits}
            <div class="p-1">
              <Skeleton variant="text" count={3} hold={false} />
              <span class="sr-only">Loading file history…</span>
            </div>
          {:else if recentCommits.length === 0}
            <div class="p-3 rounded-xl border border-edgeSoft bg-surface/30 text-center text-textMuted text-meta">
              No prior commits recorded for this path
            </div>
          {:else}
            <div class="space-y-1.5">
              {#each recentCommits as c}
                <button
                  type="button"
                  onclick={() => repoStore.inspectCommitInHistory(c.id)}
                  class="w-full text-left p-2 rounded-lg border border-edgeSoft bg-surface/40 hover:bg-surface/70 transition-colors"
                  title="Inspect {c.id.slice(0, 7)} on the graph"
                >
                  <div class="flex items-center justify-between text-micro">
                    <span class="font-mono text-accent font-semibold">{c.id.slice(0, 7)}</span>
                    <span class="text-textMuted font-mono">{formatAge(c.date, nowMs)}</span>
                  </div>
                  <div class="text-xs text-textPrimary font-medium truncate mt-0.5" title={c.message}>
                    {c.message}
                  </div>
                  <div class="text-micro text-textMuted truncate mt-0.5">
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
