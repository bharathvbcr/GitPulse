<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";


  // Survives the per-tab remount so revisiting the stack view renders the
  // last-known hierarchy instantly; the fetch then refreshes it in place.
  const stackCache = createRepoPanelCache<StackHierarchyPayload>();
</script>

<script lang="ts">
  import type {
    StackHierarchyPayload,
    StackedBranchNode,
  } from "../stack/types";
  import { repoStore } from "../stores/repoStore";
  import { harnessStore, type Guarded } from "../stores/harnessStore";
  import { invoke } from "@tauri-apps/api/core";
  import { Layers, GitBranch, RefreshCw } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { reportPanelError } from "../diagnostics/report";
  import EmptyState from "./EmptyState.svelte";

  let stackNodes: StackedBranchNode[] = $state([]);
  let stackBreadcrumb = $state<StackHierarchyPayload["breadcrumb"] | null>(null);
  let stackDefaultBranch = $state<string | null>(null);
  let isLoading = $state(false);
  let loadError = $state<string | null>(null);
  let restackError = $state<string | null>(null);
  // Latch for the one mutating action on this page. A frontend guard cancel
  // cannot kill a backend `git rebase`; the latch is what stops a second
  // click from queueing another rebase behind the first.
  let restackingKey = $state<string | null>(null);
  let inflight: AsyncGuard | null = null;
  let restackGuard: AsyncGuard | null = null;

  async function loadStack(repoPath?: string) {
    const path = repoPath ?? $repoStore.currentPath;
    if (!path) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    isLoading = true;
    restackError = null;
    try {
      const next = await invoke<StackHierarchyPayload>("cmd_get_stack_hierarchy", {
        repoPath: path,
      });
      if (!guard.isLive()) return;
      loadError = null;
      stackNodes = next.nodes;
      stackBreadcrumb = next.breadcrumb;
      stackDefaultBranch = next.default_branch;
      stackCache.set(path, next);
    } catch (err) {
      // An IPC failure must not pose as "no stacked branches": keep any last
      // good nodes and surface why the fetch failed, with a retry.
      if (!guard.isLive()) return;
      loadError = reportPanelError("stack", err);
    } finally {
      if (guard.isLive()) isLoading = false;
    }
  }

  async function restack(node: StackedBranchNode) {
    const repoPath = $repoStore.currentPath;
    if (!repoPath || !node.parent_branch_name) return;
    if (restackingKey !== null) return;
    restackGuard?.cancel();
    const guard = createAsyncGuard();
    restackGuard = guard;
    restackingKey = node.branch_name;
    // The red banner from the previous attempt must not imply this attempt
    // already failed; clear before the await, not only on success.
    restackError = null;
    try {
      // NOTE: no generic parameter on invoke() itself — the IPC contract
      // scanner only recognizes simple generics; the type rides on the const.
      const result: Guarded<string> = await invoke("cmd_restack", {
        repoPath,
        branch: node.branch_name,
        onto: node.parent_branch_name,
      });
      if (!guard.isLive() || $repoStore.currentPath !== repoPath) return;
      // README contract: mutating verdicts are recorded centrally and surface
      // in the header badge. Restack used to bypass both.
      harnessStore.recordVerdict(result.policy);
      harnessStore.recordAction({
        kind: "restack",
        label: `Restack ${node.branch_name} onto ${node.parent_branch_name}`,
        ok: true,
        verdict: result.policy,
      });
      await repoStore.refresh();
      if (!guard.isLive() || $repoStore.currentPath !== repoPath) return;
      await loadStack(repoPath);
    } catch (err) {
      if (!guard.isLive() || $repoStore.currentPath !== repoPath) return;
      restackError = reportPanelError("stack", err);
      harnessStore.recordAction({
        kind: "restack",
        label: `Restack ${node.branch_name} onto ${node.parent_branch_name}`,
        ok: false,
        verdict: null,
      });
    } finally {
      restackingKey = null;
    }
  }

  // Memo guard so unrelated store emissions do not refetch: the stack
  // reloads when the repository changes or its generation bumps (watcher
  // refreshes, branch mutations) — nothing else.
  let prevRepoPath: string | null = null;
  let prevGeneration = -1;

  $effect(() => {
    const repo = $repoStore.currentPath;
    const generation = $repoStore.generation;
    if (!repo) {
      prevRepoPath = null;
      prevGeneration = -1;
      inflight?.cancel();
      restackGuard?.cancel();
      stackNodes = [];
      stackBreadcrumb = null;
      stackDefaultBranch = null;
      loadError = null;
      restackError = null;
      isLoading = false;
      return;
    }
    if (repo === prevRepoPath && generation === prevGeneration) return;
    prevRepoPath = repo;
    prevGeneration = generation;
    // Hydrate the last-known hierarchy synchronously so a revisit renders
    // instantly; the fetch below then refreshes it in place.
    const cached = stackCache.get(repo);
    if (cached) {
      stackNodes = cached.nodes;
      stackBreadcrumb = cached.breadcrumb;
      stackDefaultBranch = cached.default_branch;
    }
    void loadStack(repo);
    const started = inflight;
    return () => {
      if (inflight === started) {
        started?.cancel();
      }
    };
  });

  $effect(() => {
    return () => {
      inflight?.cancel();
      restackGuard?.cancel();
    };
  });
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans select-none p-4 overflow-auto">
  <div class="flex items-center justify-between mb-4">
    <div class="flex items-center gap-2">
      <Layers size={18} class="text-accent" />
      <h2 class="text-sm font-semibold text-textPrimary">Code Stack Hierarchy</h2>
    </div>
    <button onclick={() => loadStack()} disabled={isLoading || restackingKey !== null} class="gp-btn">
      <RefreshCw size={13} class={isLoading ? "animate-spin" : ""} />
      <span>Refresh Stack</span>
    </button>
  </div>

  {#if stackBreadcrumb && stackBreadcrumb.breadcrumb_chain.length > 1}
    <div class="mb-3 flex items-center gap-1.5 flex-wrap font-mono text-[11px] text-textMuted">
      {#each stackBreadcrumb.breadcrumb_chain as segment, i (segment)}
        {#if i > 0}
          <span aria-hidden="true">›</span>
        {/if}
        <span class={i === stackBreadcrumb.breadcrumb_chain.length - 1 ? "text-accent font-medium" : ""}>
          {segment}
        </span>
      {/each}
    </div>
  {/if}

  {#if restackError}
    <div role="alert" class="mb-3 p-2.5 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-700 dark:text-rose-300">{restackError}</div>
  {/if}

  {#if loadError}
    <div role="alert" class="mb-3 p-2.5 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-700 dark:text-rose-300 flex items-center justify-between gap-3">
      <span class="min-w-0 truncate" title={loadError}>Failed to load stack: {loadError}</span>
      <button onclick={() => loadStack()} disabled={isLoading || restackingKey !== null} class="gp-btn shrink-0 !py-1 !px-2.5 !text-[11px]">
        <RefreshCw size={12} class={isLoading ? "animate-spin" : ""} />
        <span>Retry</span>
      </button>
    </div>
  {/if}

  {#if stackNodes.length > 0}
    <div class="space-y-3 max-w-xl">
      {#each stackNodes as node (node.branch_name)}
        <div
          aria-busy={restackingKey === node.branch_name}
          class="p-3.5 bg-surface border border-border/70 rounded-2xl shadow-card flex items-center justify-between transition-[border-color,box-shadow] duration-150 hover:border-accent/40 {node.branch_name === $repoStore.currentBranch ? 'border-accent/50 ring-1 ring-accent/30' : ''}"
        >
          <div class="flex items-center gap-3 min-w-0">
            <GitBranch size={16} class={node.branch_name === $repoStore.currentBranch ? "text-accent" : "text-textMuted"} />
            <div class="min-w-0">
              <div class="font-medium text-textPrimary text-sm flex items-center gap-2">
                <span class="truncate">{node.branch_name}</span>
                {#if node.branch_name === $repoStore.currentBranch}
                  <span class="text-[10px] bg-accent/20 text-accent px-1.5 py-0.5 rounded-full font-mono">HEAD</span>
                {/if}
                {#if node.parent_branch_name === null && node.branch_name === stackDefaultBranch}
                  <span class="text-[10px] bg-border/40 text-textMuted px-1.5 py-0.5 rounded-full font-mono">ROOT</span>
                {/if}
              </div>
              <span class="text-textMuted text-[11px] font-mono">
                {node.parent_branch_name ? `Based on ${node.parent_branch_name} (+${node.commit_count_ahead_of_parent})` : "Root branch"}
                {#if node.child_branch_names.length > 0}
                  · {node.child_branch_names.length} downstream
                {/if}
              </span>
            </div>
          </div>

          <div class="flex items-center gap-1.5 shrink-0">
            {#if node.parent_branch_name}
              <button
                onclick={() => restack(node)}
                disabled={restackingKey !== null}
                class="gp-btn !py-1 !px-2.5 !text-[11px]"
              >
                {#if restackingKey === node.branch_name}
                  <RefreshCw size={12} class="animate-spin" />
                {/if}
                <span>{restackingKey === node.branch_name ? "Restacking…" : "Restack"}</span>
              </button>
            {/if}
            <button
              onclick={() => repoStore.checkoutBranch(node.branch_name)}
              disabled={restackingKey !== null}
              class="gp-btn !py-1 !px-2.5 !text-[11px]"
            >
              Checkout
            </button>
          </div>
        </div>
      {/each}
    </div>
  {:else if isLoading}
    <div class="space-y-3 max-w-xl" aria-hidden="true">
      <div class="p-3.5 bg-surface border border-border/70 rounded-2xl animate-pulse h-16"></div>
      <div class="p-3.5 bg-surface border border-border/70 rounded-2xl animate-pulse h-16 w-11/12"></div>
    </div>
    <p class="sr-only">Loading stack hierarchy…</p>
  {:else if !loadError}
    <div class="flex-1 flex">
      <EmptyState
        icon={Layers}
        title="No stacked branches"
        hint="Stacked branch hierarchies detected in this repository will appear here."
      />
    </div>
  {/if}
</div>
