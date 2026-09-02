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
  } from "lucide-svelte";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { loadWork } from "../work/load";
  import {
    degradedSummary,
    noteworthyStatuses,
    UNBOUND_ROW_ID,
    type WorkRow,
  } from "../work/projection";
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
    allowed: "text-textMuted bg-surfaceHover",
    demoted: "text-amber-400 bg-amber-500/10",
    granted: "text-sky-400 bg-sky-500/10",
    widened: "text-amber-400 bg-amber-500/10",
    degraded: "text-amber-400 bg-amber-500/10",
    warned: "text-amber-400 bg-amber-500/10",
    blocked: "text-rose-400 bg-rose-500/10",
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
    if (row.taskId === UNBOUND_ROW_ID) return "Not bound to a task";
    return row.title || row.taskId;
  }

  const degraded = $derived(projection ? degradedSummary(projection.sources) : "");
</script>

<div class="flex-1 overflow-y-auto p-4 font-sans text-[12px] text-textPrimary">
  <div class="flex items-center justify-between mb-3 max-w-5xl">
    <h2 class="flex items-center gap-2 text-[13px] font-semibold">
      <LayoutGrid size={15} class="text-accent" />
      Work
      <span class="text-textMuted font-normal text-[11px]">
        tasks, worktrees, pull requests, runs and verdicts, joined
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
      class="mb-3 max-w-5xl flex items-start gap-2 rounded-xl border border-amber-500/30 bg-amber-500/10 p-2.5 text-[11px] text-amber-300"
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
    <EmptyState
      icon={LayoutGrid}
      title="Nothing in flight"
      hint={projection.sources.tasks.present
        ? "No tasks are leased, no worktrees are open and nothing has been recorded yet."
        : "This repository has no DevCouncil task store, so there is no task model to project."}
    />
  {:else if projection}
    <div class="max-w-5xl space-y-2">
      {#each projection.rows as row (row.taskId || "__unbound")}
        {@const chips = noteworthyStatuses(row.verdicts)}
        <div
          class="rounded-2xl border border-border/70 bg-surface p-3 shadow-card"
          class:opacity-80={row.taskId === UNBOUND_ROW_ID}
        >
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <div class="flex items-center gap-2 flex-wrap font-medium">
                <span class="truncate">{rowTitle(row)}</span>
                {#if row.taskId}
                  <span class="font-mono text-[10px] text-textMuted">{row.taskId}</span>
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
                <span class="flex items-center gap-1" title="Worktrees">
                  <Trees size={12} />
                  {row.worktrees.length}
                </span>
                <span class="flex items-center gap-1" title="Open pull requests">
                  <GitPullRequest size={12} />
                  {row.pullRequests.length}
                </span>
                <span class="flex items-center gap-1" title="Workflow runs">
                  <Play size={12} />
                  {row.runs.length}
                </span>
                <span class="flex items-center gap-1" title="Grants applied">
                  <ShieldCheck size={12} />
                  {row.grants.length}
                </span>
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
                  class="rounded bg-rose-500/10 px-1.5 py-0.5 text-[10px] font-medium text-rose-400"
                  title="Verdicts this build could not read. Not counted as allowed."
                >
                  unreadable {row.verdicts.unparsed}
                </span>
              {/if}
            </div>
          </div>

          {#if row.worktrees.length > 0 || row.pullRequests.length > 0}
            <div class="mt-2.5 grid gap-2 border-t border-border/50 pt-2.5 md:grid-cols-2">
              {#if row.worktrees.length > 0}
                <ul class="space-y-1">
                  {#each row.worktrees as binding (binding.worktree.path)}
                    <li class="flex items-center gap-1.5 font-mono text-[10px] text-textMuted">
                      <GitBranch size={11} class="shrink-0" />
                      <span class="truncate" title={binding.worktree.path}>
                        {binding.worktree.branch ?? "(detached)"}
                      </span>
                      {#if binding.worktree.dirty_files}
                        <span class="text-amber-400">·{binding.worktree.dirty_files} dirty</span>
                      {/if}
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
</div>
