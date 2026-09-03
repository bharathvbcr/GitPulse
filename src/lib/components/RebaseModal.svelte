<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { filterStore } from "../stores/filterStore";
  import { invoke } from "@tauri-apps/api/core";
  import { fade, scale } from "svelte/transition";
  import {
    backdropFade,
    backdropFadeOut,
    cardScale,
    cardScaleOut,
  } from "../ui/transitions";
  import { seedRebasePlan, shouldReseed } from "../rebase/planner";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { reportPanelError } from "../diagnostics/report";
  import { GitMerge, Check, AlertCircle } from "lucide-svelte";

  let {
    isOpen = false,
    onClose,
  }: {
    isOpen?: boolean;
    onClose?: () => void;
  } = $props();

  interface EditableRebaseItem {
    id: string;
    action: "Pick" | "Squash" | "Fixup" | "Drop" | "Reword";
    summary: string;
  }

  let items = $state<EditableRebaseItem[]>([]);
  let ontoBranch = $state("main");
  let isExecuting = $state(false);
  let errorMsg = $state<string | null>(null);

  type RebaseActionPayload =
    | "Pick"
    | "Squash"
    | "Fixup"
    | "Drop"
    | { Reword: string };

  let wasOpen = false;
  let planDirty = $state(false);
  let seededSignature = "";
  const planSignature = (commits: typeof $graphStore.commits) =>
    commits
      .slice(0, 12)
      .map((c) => c.id)
      .join(",");

  $effect(() => {
    const current = planSignature($graphStore.commits);
    // Rebuild the plan only when it cannot destroy user work: on opening, or
    // while pristine and the underlying history actually moved.
    if (shouldReseed({ isOpen, wasOpen, dirty: planDirty, currentSignature: current, seededSignature })) {
      errorMsg = null;
      ontoBranch = $repoStore.defaultBranch || "main";
      items = seedRebasePlan($graphStore.commits);
      seededSignature = current;
      planDirty = false;
    }
    wasOpen = isOpen;
  });

  async function executeRebase() {
    // Capture the repo once: a tab switch while the rebase runs must not
    // redirect the completion effects ($repoStore.currentPath re-read after
    // the awaits would target whatever tab is now active).
    const repoPath = $repoStore.currentPath;
    if (!repoPath || items.length === 0) return;
    isExecuting = true;
    errorMsg = null;

    try {
      const steps = items.map((it) => {
        const action: RebaseActionPayload =
          it.action === "Reword" ? { Reword: it.summary } : it.action;
        return {
          commit_id: it.id,
          action,
        };
      });

      await invoke("cmd_rebase_interactive", {
        repoPath,
        ontoCommit: ontoBranch,
        steps,
      });

      // Both effects are scoped to the captured path, so they land on the
      // rebased repo's session even if the active tab moved mid-run.
      await repoStore.refresh(repoPath);
      // The bare loadGraph(repoPath) form reset the view to query=""/HEAD
      // while FilterBar still showed the selected filter, and the scheduler
      // memo then blocked the correction. Reload with the visible context;
      // the backend applies every query term.
      await graphStore.loadGraph(repoPath, $filterStore.searchQuery, $filterStore.selectedBranch);
      onClose?.();
    } catch (err: unknown) {
      errorMsg = reportPanelError("rebase", err);
    } finally {
      isExecuting = false;
    }
  }

  /** Mid-run dismissal hides progress and completes invisibly later. */
  function requestClose() {
    if (isExecuting) return;
    onClose?.();
  }
</script>

{#if isOpen}
  <div
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    onclick={(e) => e.target === e.currentTarget && requestClose()}
    onkeydown={(e) => e.key === "Escape" && requestClose()}
    in:fade={backdropFade()}
    out:fade={backdropFadeOut()}
    class="fixed inset-0 bg-black/40 backdrop-blur-sm flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <!-- Modal Card -->
    <div
      use:trapFocus
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-xl gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 border-b border-border/60 flex items-center justify-between">
        <div class="flex items-center gap-2 text-sm font-semibold text-textPrimary">
          <GitMerge size={16} class="text-accent" />
          <span>Interactive Rebase</span>
        </div>
        <div class="flex items-center gap-2">
          <span class="text-textMuted font-mono">Onto:</span>
          <input
            type="text"
            bind:value={ontoBranch}
            class="gp-field !w-28 font-mono"
          />
        </div>
      </div>

      {#if errorMsg}
        <div class="mx-4 mt-3 p-2 bg-rose-500/10 border border-rose-500/30 rounded-xl text-rose-400 text-xs flex items-center gap-2">
          <AlertCircle size={14} class="shrink-0" />
          <span>{errorMsg}</span>
        </div>
      {/if}

      <div class="p-4 space-y-2 max-h-80 overflow-y-auto">
        {#each items as commit}
          <div class="flex items-center gap-3 p-2.5 bg-background border border-border/70 rounded-xl text-xs">
            <span class="font-mono text-accent w-20 truncate" title={commit.id}>{commit.id.substring(0, 8)}</span>
            <select
              bind:value={commit.action}
              onchange={() => (planDirty = true)}
              class="bg-surface border border-border/80 rounded-lg px-2 py-1 text-xs text-textPrimary focus:outline-none focus:border-accent/60 font-medium transition-colors"
            >
              <option value="Pick">pick</option>
              <option value="Squash">squash</option>
              <option value="Fixup">fixup</option>
              <option value="Reword">reword</option>
              <option value="Drop">drop</option>
            </select>
            <input
              type="text"
              bind:value={commit.summary}
              oninput={() => (planDirty = true)}
              class="flex-1 bg-transparent border-b border-transparent focus:border-border text-xs text-textPrimary focus:outline-none px-1"
            />
          </div>
        {/each}
      </div>

      <div class="p-4 border-t border-border/60 bg-surfaceHover/30 flex justify-end gap-2">
        <button onclick={requestClose} disabled={isExecuting} class="gp-btn">Cancel</button>
        <button
          onclick={executeRebase}
          disabled={isExecuting || items.length === 0}
          class="gp-btn-primary"
        >
          <Check size={14} />
          <span>{isExecuting ? "Rebasing..." : "Execute Rebase"}</span>
        </button>
      </div>
    </div>
  </div>
{/if}

