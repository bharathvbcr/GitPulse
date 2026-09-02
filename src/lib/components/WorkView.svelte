<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import type { WorkProjection } from "../work/projection";

  // Survives the per-tab remount so revisiting Work renders the last join
  // instantly; the fetch below then refreshes it in place.
  const workCache = createRepoPanelCache<WorkProjection>();
</script>

<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { openExternal } from "../desktop/openExternal";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import {
    AlertTriangle,
    ExternalLink,
    GitBranch,
    GitPullRequest,
    LayoutGrid,
    Play,
    RefreshCw,
    ShieldCheck,
    Trees,
    Bot,
    FileDiff,
    GitMerge,
    ChevronRight,
  } from "lucide-svelte";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";
  import RepoPanel from "./RepoPanel.svelte";
  import { loadWork } from "../work/load";
  import {
    degradedSummary,
    dirtyCount,
    noteworthyStatuses,
    type WorkRow,
  } from "../work/projection";
  import { headline, kindTitle } from "../repos/operation";
  import { isAgentWorktree } from "../work/agentWorktree";
  import type { PolicyStatus } from "../stores/harnessStore";

  let projection = $state<WorkProjection | null>(null);
  let loading = $state(false);
  let guard: AsyncGuard | null = null;

  /**
   * Tone per policy status. Spelled out rather than derived from the name so
   * a status added to the union without a colour here is a compile-time gap,
   * not a chip that silently renders as the default.
   */
  const STATUS_TONE: Record<PolicyStatus, string> = {
    // Each carries both shades: a single fixed one is legible in one theme
    // only, and these chips are the exceptions the reader is here to notice.
    allowed: "text-textMuted bg-surfaceHover",
    demoted: "text-amber-700 dark:text-amber-400 bg-amber-500/10",
    granted: "text-sky-700 dark:text-sky-400 bg-sky-500/10",
    widened: "text-amber-700 dark:text-amber-400 bg-amber-500/10",
    degraded: "text-amber-700 dark:text-amber-400 bg-amber-500/10",
    warned: "text-amber-700 dark:text-amber-400 bg-amber-500/10",
    blocked: "text-rose-700 dark:text-rose-400 bg-rose-500/10",
    unchecked: "text-textMuted bg-surfaceHover",
  };

  async function refresh(repo: string) {
    guard?.cancel();
    guard = createAsyncGuard();
    const run = guard;
    loading = true;
    const result = await loadWork(repo);
    if (!run.isLive()) return;
    projection = result;
    workCache.set(repo, result);
    loading = false;
  }

  let previousRepo: string | null = null;
  $effect(() => {
    const repo = $repoStore.currentPath;
    if (repo === previousRepo) return;
    previousRepo = repo;
    projection = repo ? (workCache.get(repo) ?? null) : null;
    if (repo) void refresh(repo);
  });

  $effect(() => () => guard?.cancel());

  function rowTitle(row: WorkRow): string {
    // "Not bound to a task" is only meaningful where tasks exist. In a
    // repository with no task store the catch-all holds pull requests and
    // runs whose branch no worktree has checked out — say that instead.
    if (row.kind === "unbound") {
      return hasTasks ? "Not bound to a task" : "Not checked out anywhere";
    }
    return row.title || row.taskId || "(untitled)";
  }

  const hasTasks = $derived(projection?.sources.tasks.present === true);

  /**
   * Opens a row's worktree as a repository tab.
   *
   * This is what makes the screen a workspace rather than a report: the whole
   * point of showing that a worktree is stuck mid-rebase is being one click
   * from the Resolve view that unsticks it. Rows with several worktrees open
   * the first — the one the row is keyed on in worktree mode.
   */
  async function openWorktree(row: WorkRow): Promise<void> {
    const path = row.worktrees[0]?.worktree.path;
    if (!path) return;
    await repoStore.openRepo(path);
    // A parked operation is resolved in the Resolve view; anything else is
    // most usefully seen as its working-tree diff.
    repoStore.setActiveTab(row.operation ? "conflict" : "diff");
  }

  const degraded = $derived(projection ? degradedSummary(projection.sources) : "");

  /**
   * Remotes, submodules and the stash, folded in from what used to be a
   * separate Repo view.
   *
   * They belong on this page — "where does this push to, why is that folder
   * empty, what did I put aside" are questions about the same repository this
   * screen is already describing — but they are reference material, not work
   * in flight, so they start collapsed and never push the rows down.
   */
  let showRepoDetail = $state(false);
</script>

<div class="flex-1 overflow-y-auto p-4 font-sans text-[12px] text-textPrimary">
  <div class="flex items-center justify-between mb-3 max-w-5xl">
    <h2 class="flex items-center gap-2 text-[13px] font-semibold">
      <LayoutGrid size={15} class="text-accent" />
      Work
      <span class="text-textMuted font-normal text-[11px]">
        {hasTasks
          ? "tasks, worktrees, pull requests, runs and verdicts, joined"
          : "every worktree in flight, with its changes, pull requests and runs"}
      </span>
    </h2>
    <button
      type="button"
      class="flex items-center gap-1.5 px-2 py-1 rounded-lg border border-border/70 hover:bg-surfaceHover text-[11px] disabled:opacity-50"
      disabled={loading || !$repoStore.currentPath}
      onclick={() => $repoStore.currentPath && void refresh($repoStore.currentPath)}
    >
      <RefreshCw size={12} class={loading ? "animate-spin" : ""} />
      Refresh
    </button>
  </div>

  <!-- Stated before the rows, not after them. A join assembled from a source
       that could not be read looks exactly like one assembled from a source
       that was empty, and the reader has to know which they are looking at
       before they start reading it. -->
  {#if degraded}
    <div
      class="mb-3 max-w-5xl flex items-start gap-2 rounded-xl border border-amber-500/30 bg-amber-500/10 p-2.5 text-[11px] text-amber-700 dark:text-amber-300"
    >
      <AlertTriangle size={14} class="shrink-0 mt-px" />
      <span>{degraded}</span>
    </div>
  {/if}

  {#if !$repoStore.currentPath}
    <EmptyState icon={LayoutGrid} title="No repository open" hint="Open a repository to see the work in it." />
  {:else if loading && !projection}
    <div class="max-w-5xl space-y-2">
      <Skeleton />
      <Skeleton />
      <Skeleton />
    </div>
  {:else if projection && projection.rows.length === 0}
    <!-- Reaching here means git listed no worktrees at all, which is close to
         impossible for an open repository — every repository has at least its
         own. The old text blamed a missing DevCouncil store, which named a
         system most readers do not run and offered them nothing to do. -->
    <EmptyState
      icon={LayoutGrid}
      title="Nothing in flight"
      hint={projection.sources.worktrees.ok
        ? "No worktrees, pull requests or runs were found for this repository."
        : "The worktree list could not be read, so this screen cannot say what is in flight."}
    />
  {:else if projection}
    <div class="max-w-5xl space-y-2">
      {#each projection.rows as row (row.key || "__unbound")}
        {@const chips = noteworthyStatuses(row.verdicts)}
        <div
          class="rounded-2xl border border-border/70 bg-surface p-3 shadow-card"
          class:opacity-80={row.kind === "unbound"}
        >
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <div class="flex items-center gap-2 flex-wrap font-medium">
                <span class="truncate">{rowTitle(row)}</span>
                {#if row.taskId}
                  <span class="font-mono text-[10px] text-textMuted">{row.taskId}</span>
                {/if}
                <!-- An agent worktree and a hand-made one want opposite
                     remedies — merge or resume, versus prune — so they are
                     never labelled the same. -->
                {#if row.worktrees.some((b) => isAgentWorktree(b.worktree.path))}
                  <span
                    class="inline-flex items-center gap-1 rounded-full bg-accent/10 px-1.5 py-0.5 text-[10px] text-accent"
                    title="A worktree Claude Code created for a session"
                  >
                    <Bot size={10} />agent
                  </span>
                {/if}
                {#if row.worktrees.some((b) => b.worktree.is_main)}
                  <span class="rounded-full bg-surfaceHover px-1.5 py-0.5 text-[10px] text-textMuted">
                    main worktree
                  </span>
                {/if}
                {#if row.lease}
                  <span
                    class="rounded-full bg-sky-500/10 px-1.5 py-0.5 text-[10px] text-sky-400"
                    title="Leased by {row.lease.owner}{row.lease.agent
                      ? ` (${row.lease.agent})`
                      : ''}{row.lease.expires_at
                      ? `, expires ${row.lease.expires_at}`
                      : ', never expires'}"
                  >
                    {row.lease.status}
                  </span>
                {/if}
              </div>
              <div class="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-textMuted">
                <!-- A worktree row IS one worktree; printing "1" beside every
                     one of them is noise that crowds out the counts that vary. -->
                {#if row.kind !== "worktree"}
                  <span class="flex items-center gap-1" title="Worktrees">
                    <Trees size={12} />
                    {row.worktrees.length}
                  </span>
                {/if}
                <span class="flex items-center gap-1" title="Open pull requests">
                  <GitPullRequest size={12} />
                  {row.pullRequests.length}
                </span>
                <span class="flex items-center gap-1" title="Workflow runs">
                  <Play size={12} />
                  {row.runs.length}
                </span>
                <!-- Grants are a DevCouncil concept. Showing "0" of them to a
                     reader who runs no store is a column that can only ever
                     say zero. -->
                {#if row.grants.length > 0}
                  <span class="flex items-center gap-1" title="Grants applied">
                    <ShieldCheck size={12} />
                    {row.grants.length}
                  </span>
                {/if}
                <!-- -1 means the count was never taken (bare, or past the scan
                     cap). Rendering that as 0 would report an unscanned
                     worktree as verified clean. -->
                {#if dirtyCount(row) > 0}
                  <span class="flex items-center gap-1 text-amber-500 dark:text-amber-400" title="Uncommitted files">
                    <FileDiff size={12} />
                    {dirtyCount(row)} uncommitted
                  </span>
                {:else if dirtyCount(row) === 0}
                  <span class="flex items-center gap-1" title="No uncommitted changes">
                    <FileDiff size={12} />
                    clean
                  </span>
                {/if}
                <span class="font-mono" title="Ledger events attributed to this row">
                  {row.verdicts.events} events
                </span>
              </div>
            </div>

            <div class="flex flex-wrap items-center justify-end gap-1 shrink-0">
              {#each chips as [status, count] (status)}
                <span class="rounded px-1.5 py-0.5 text-[10px] font-medium {STATUS_TONE[status]}">
                  {status} {count}
                </span>
              {/each}
              <!-- A verdict the ledger recorded and this build could not read.
                   Never folded into `allowed`: that is the exact shape of a
                   check that could not run reading as one that passed. -->
              {#if row.verdicts.unparsed > 0}
                <span
                  class="rounded bg-rose-500/10 px-1.5 py-0.5 text-[10px] font-medium text-rose-700 dark:text-rose-400"
                  title="Verdicts this build could not read. Not counted as allowed."
                >
                  unreadable {row.verdicts.unparsed}
                </span>
              {/if}
            </div>
          </div>

          <!-- A worktree stopped mid-rebase is blocked on a person, and it
               is the only thing on this screen that cannot make progress on
               its own. It gets a line of its own, above the counts. -->
          {#if row.operation}
            <button
              type="button"
              class="mt-2 flex w-full items-start gap-2 rounded-xl border border-amber-500/40 bg-amber-500/10 px-2.5 py-1.5 text-left text-[11px] text-amber-600 hover:bg-amber-500/20 dark:text-amber-300"
              onclick={() => openWorktree(row)}
              title="Open this worktree to finish or abort the {kindTitle(row.operation.kind).toLowerCase()}"
            >
              <GitMerge size={12} class="mt-px shrink-0" />
              <span class="min-w-0">
                {headline(row.operation)}
                {#if row.operation.conflicted_total > 0}
                  — {row.operation.conflicted_total} file{row.operation.conflicted_total === 1
                    ? ""
                    : "s"} still conflicted
                {/if}
              </span>
            </button>
          {/if}

          {#if row.worktrees.length > 0 || row.pullRequests.length > 0}
            <div class="mt-2.5 grid gap-2 border-t border-border/50 pt-2.5 md:grid-cols-2">
              {#if row.worktrees.length > 0}
                <ul class="space-y-1">
                  {#each row.worktrees as binding (binding.worktree.path)}
                    <li>
                      <button
                        type="button"
                        class="flex w-full items-center gap-1.5 rounded font-mono text-[10px] text-textMuted hover:text-accent"
                        title="Open {binding.worktree.path}"
                        onclick={() => void repoStore.openRepo(binding.worktree.path)}
                      >
                        <GitBranch size={11} class="shrink-0" />
                        <span class="truncate">{binding.worktree.branch ?? "(detached)"}</span>
                        {#if binding.worktree.dirty_files}
                          <span class="text-amber-500 dark:text-amber-400"
                            >·{binding.worktree.dirty_files} dirty</span
                          >
                        {/if}
                        {#if binding.worktree.is_locked}
                          <span class="text-textMuted">·locked</span>
                        {/if}
                      </button>
                    </li>
                  {/each}
                </ul>
              {/if}
              {#if row.pullRequests.length > 0}
                <ul class="space-y-1">
                  {#each row.pullRequests as pr (pr.number)}
                    <li class="flex items-center gap-1.5 text-[11px]">
                      <GitPullRequest size={11} class="shrink-0 text-accent" />
                      <span class="truncate">#{pr.number} {pr.title}</span>
                      <button
                        type="button"
                        class="shrink-0 text-textMuted hover:text-accent"
                        aria-label={`Open PR #${pr.number} on GitHub`}
                        onclick={() => void openExternal(pr.url)}
                      >
                        <ExternalLink size={11} />
                      </button>
                    </li>
                  {/each}
                </ul>
              {/if}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}

  {#if $repoStore.currentPath}
    <div class="mt-4 max-w-5xl">
      <button
        type="button"
        class="flex w-full items-center gap-1.5 rounded-xl border border-border/70 px-3 py-2 text-[11px] font-medium text-textMuted hover:bg-surfaceHover"
        onclick={() => (showRepoDetail = !showRepoDetail)}
        aria-expanded={showRepoDetail}
      >
        <ChevronRight
          size={13}
          class="shrink-0 transition-transform {showRepoDetail ? 'rotate-90' : ''}"
        />
        Remotes, submodules and stash
      </button>
      {#if showRepoDetail}
        <div class="mt-2">
          <RepoPanel embedded />
        </div>
      {/if}
    </div>
  {/if}
</div>
