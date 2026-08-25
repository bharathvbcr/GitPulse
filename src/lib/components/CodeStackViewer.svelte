<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { Layers, GitBranch, RefreshCw } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import EmptyState from "./EmptyState.svelte";

  interface StackNode {
    branch_name: string;
    parent_branch_name?: string | null;
    commit_count_ahead_of_parent: number;
    tip_commit_id: string;
    child_branch_names: string[];
  }

  let stackNodes: StackNode[] = $state([]);
  let isLoading = $state(false);
  let loadError = $state<string | null>(null);
  let restackError = $state<string | null>(null);
  let inflight: AsyncGuard | null = null;
  let restackGuard: AsyncGuard | null = null;

  function defaultStackRoot(): string {
    const locals = $repoStore.branches.filter((b) => !b.is_remote).map((b) => b.name);
    if (locals.includes("main")) return "main";
    if (locals.includes("master")) return "master";
    return locals[0] || "main";
  }

  async function loadStack(repoPath?: string) {
    const path = repoPath ?? $repoStore.currentPath;
    if (!path) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    isLoading = true;
    restackError = null;
    const defaultBranch = defaultStackRoot();
    try {
      const next = await invoke<StackNode[]>("cmd_get_stack_hierarchy", {
        repoPath: path,
        defaultBranch,
      });
      if (!guard.isLive()) return;
      loadError = null;
      stackNodes = next;
    } catch (err) {
      // An IPC failure must not pose as "no stacked branches": keep any last
      // good nodes and surface why the fetch failed, with a retry.
      if (!guard.isLive()) return;
      loadError = String(err);
    } finally {
      if (guard.isLive()) isLoading = false;
    }
  }

  async function restack(node: StackNode) {
    const repoPath = $repoStore.currentPath;
    if (!repoPath || !node.parent_branch_name) return;
    restackGuard?.cancel();
    const guard = createAsyncGuard();
    restackGuard = guard;
    try {
      await invoke("cmd_restack", {
        repoPath,
        branch: node.branch_name,
        onto: node.parent_branch_name,
      });
      if (!guard.isLive() || $repoStore.currentPath !== repoPath) return;
      await repoStore.refresh();
      if (!guard.isLive() || $repoStore.currentPath !== repoPath) return;
      await loadStack(repoPath);
    } catch (err) {
      if (!guard.isLive() || $repoStore.currentPath !== repoPath) return;
      restackError = String(err);
    }
  }

  $effect(() => {
    return () => {
      inflight?.cancel();
      restackGuard?.cancel();
    };
  });

  $effect(() => {
    const repo = $repoStore.currentPath;
    if (!repo) {
      inflight?.cancel();
      restackGuard?.cancel();
      stackNodes = [];
      loadError = null;
      restackError = null;
      isLoading = false;
      return;
    }
    void loadStack(repo);
    const started = inflight;
    return () => {
      if (inflight === started) {
        started?.cancel();
      }
    };
  });
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans select-none p-4 overflow-auto">
  <div class="flex items-center justify-between mb-4">
    <div class="flex items-center gap-2">
      <Layers size={18} class="text-accent" />
      <h2 class="text-sm font-semibold text-textPrimary">Code Stack Hierarchy</h2>
    </div>
    <button onclick={() => loadStack()} class="gp-btn">
      <RefreshCw size={13} class={isLoading ? "animate-spin" : ""} />
      <span>Refresh Stack</span>
    </button>
  </div>

  {#if restackError}
    <div class="mb-3 p-2.5 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-300">{restackError}</div>
  {/if}

  {#if loadError}
    <div class="mb-3 p-2.5 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-300 flex items-center justify-between gap-3">
      <span class="min-w-0 truncate" title={loadError}>Failed to load stack: {loadError}</span>
      <button onclick={() => loadStack()} class="gp-btn shrink-0 !py-1 !px-2.5 !text-[11px]">
        <RefreshCw size={12} class={isLoading ? "animate-spin" : ""} />
        <span>Retry</span>
      </button>
    </div>
  {/if}

  {#if stackNodes.length === 0 && !loadError}
    <div class="flex-1 flex">
      <EmptyState
        icon={Layers}
        title="No stacked branches"
        hint="Stacked branch hierarchies detected in this repository will appear here."
      />
    </div>
  {:else if stackNodes.length > 0}
    <div class="space-y-3 max-w-xl">
      {#each stackNodes as node}
        <div class="p-3.5 bg-surface border border-border/70 rounded-2xl shadow-card flex items-center justify-between transition-[border-color,box-shadow] duration-150 hover:border-accent/40 {node.branch_name === $repoStore.currentBranch ? 'border-accent/50 ring-1 ring-accent/30' : ''}">
          <div class="flex items-center gap-3 min-w-0">
            <GitBranch size={16} class={node.branch_name === $repoStore.currentBranch ? "text-accent" : "text-textMuted"} />
            <div class="min-w-0">
              <div class="font-medium text-textPrimary text-sm flex items-center gap-2">
                <span class="truncate">{node.branch_name}</span>
                {#if node.branch_name === $repoStore.currentBranch}
                  <span class="text-[10px] bg-accent/20 text-accent px-1.5 py-0.5 rounded-full font-mono">HEAD</span>
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
              <button onclick={() => restack(node)} class="gp-btn !py-1 !px-2.5 !text-[11px]">Restack</button>
            {/if}
            <button onclick={() => repoStore.checkoutBranch(node.branch_name)} class="gp-btn !py-1 !px-2.5 !text-[11px]">
              Checkout
            </button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>
