<script lang="ts">
  import { untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { guardedDismiss } from "./modalGuard";
  import { graphStore } from "../stores/graphStore";
  import { fade, scale } from "svelte/transition";
  import {
    backdropFade,
    backdropFadeOut,
    cardScale,
    cardScaleOut,
  } from "../ui/transitions";
  import { rebasePlanProblem, seedRebasePlan, shouldReseed, type RebaseStep } from "../rebase/planner";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { scrollEdgeFade } from "../ui/scrollEdgeFade";
  import { reportPanelError } from "../diagnostics/report";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { GitMerge, Check, AlertCircle } from "@lucide/svelte";

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

  let planRepoPath = $state<string | null>(null);
  let planGeneration = $state(0);
  const planCurrent = $derived(planRepoPath === $repoStore.currentPath && planGeneration === $repoStore.generation);

  const planProblem = $derived(rebasePlanProblem(items));

  let wasOpen = false;
  let planDirty = $state(false);
  let seededSignature = "";
  const planSignature = (commits: typeof $graphStore.commits) =>
    commits
      .slice(0, 12)
      .map((c) => c.id)
      .join(",");

  const repoGeneration = $derived($repoStore.generation);
  const defaultBranch = $derived($repoStore.defaultBranch);
  $effect(() => {
    const current = planSignature($graphStore.commits);
    // Rebuild the plan only when it cannot destroy user work: on opening, or
    // while pristine and the underlying history actually moved.
    if ((!wasOpen || planCurrent) && shouldReseed({ isOpen, wasOpen, dirty: planDirty, currentSignature: current, seededSignature })) {
      untrack(() => {
        planRepoPath = $currentRepoPath;
        planGeneration = repoGeneration;
        errorMsg = null;
        ontoBranch = defaultBranch || "main";
        items = seedRebasePlan($graphStore.commits);
        seededSignature = current;
        planDirty = false;
      });
    }
    wasOpen = isOpen;
  });

  async function executeRebase() {
    // Capture the repo once: a tab switch while the rebase runs must not
    // redirect the completion effects ($repoStore.currentPath re-read after
    // the awaits would target whatever tab is now active).
    const repoPath = $repoStore.currentPath;
    if (!repoPath || items.length === 0 || isExecuting || !planCurrent || planProblem !== null) return;
    isExecuting = true;
    errorMsg = null;

    try {
      const steps = items.map((it) => {
        const action: RebaseStep["action"] =
          it.action === "Reword" ? { Reword: it.summary } : it.action;
        return {
          commit_id: it.id,
          action,
        };
      });

      const result = await repoStore.rebaseInteractive(ontoBranch, steps);
      if (!result.ok) {
        const failure = describeFailure({ verb: "complete", object: "the rebase" }, result.error ?? "");
        errorMsg = result.error ? `${failure.headline} ${failure.detail ?? result.error}` : failure.headline;
        return;
      }
      onClose?.();
    } catch (err: unknown) {
      const reported = reportPanelError("rebase", err);
      errorMsg = `${describeFailure({ verb: "complete", object: "the rebase" }, err).headline} ${reported}`;
    } finally {
      isExecuting = false;
    }
  }

  /** Mid-run dismissal hides progress and completes invisibly later. */
  function requestClose() {
    guardedDismiss(isExecuting, onClose);
  }
</script>

{#if isOpen}
  <div
    role="dialog"
    aria-modal="true"
    aria-labelledby="rebase-modal-title"
    tabindex="-1"
    onclick={(e) => e.target === e.currentTarget && requestClose()}
    onkeydown={(e) => e.key === "Escape" && requestClose()}
    in:fade={backdropFade()}
    out:fade={backdropFadeOut()}
    class="gp-scrim bg-black/40 flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <!-- Modal Card -->
    <div
      use:trapFocus
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-xl gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 flex items-center justify-between">
        <h2 id="rebase-modal-title" class="flex items-center gap-2 text-sm font-semibold text-textPrimary">
          <GitMerge size={16} class="text-accent" />
          <span>Interactive rebase</span>
        </h2>
        <div class="flex items-center gap-2">
          <label for="rebase-onto" class="gp-label font-mono">Onto</label>
          <input
            id="rebase-onto"
            type="text"
            bind:value={ontoBranch}
            class="gp-field w-28! font-mono"
          />
        </div>
      </div>

      {#if !planCurrent || planProblem || errorMsg}
        <div class="flex flex-col gap-2 px-4 pb-1">
          {#if !planCurrent}
            <p role="alert" class="gp-notice" data-tone="warn">Repository changed. Close and reopen this plan before rebasing.</p>
          {/if}
          {#if planProblem}
            <p role="alert" class="gp-notice" data-tone="warn">{planProblem}</p>
          {/if}
          {#if errorMsg}
            <div class="gp-notice" data-tone="danger">
              <AlertCircle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" />
              <span class="min-w-0 flex-1 break-words">{errorMsg}</span>
            </div>
          {/if}
        </div>
      {/if}

      <div class="p-4 space-y-2 max-h-80 overflow-y-auto scroll-py-8" data-dialog-body use:scrollEdgeFade>
        {#each items as commit}
          <div class="flex items-center gap-3 p-2.5 bg-background rounded-xl text-xs">
            <span class="font-mono text-accent w-20 truncate" title={commit.id}>{commit.id.substring(0, 8)}</span>
            <select
              bind:value={commit.action}
              onchange={() => (planDirty = true)}
              aria-label="Action for {commit.id.substring(0, 8)}"
              class="gp-select"
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
              aria-label="Summary for {commit.id.substring(0, 8)}"
              class="gp-field min-w-0 flex-1"
            />
          </div>
        {/each}
      </div>

      <div class="p-4 bg-surfaceHover/30 flex justify-end gap-2">
        <button type="button" onclick={requestClose} disabled={isExecuting} class="gp-btn" data-variant="secondary">Cancel</button>
        <button
          type="button"
          onclick={executeRebase}
          disabled={isExecuting || items.length === 0 || !planCurrent || planProblem !== null}
          class="gp-btn"
          data-variant="primary"
        >
          <Check size={14} />
          <span>{isExecuting ? "Rebasing…" : "Start rebase"}</span>
        </button>
      </div>
    </div>
  </div>
{/if}
