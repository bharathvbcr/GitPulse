<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { AlertTriangle, ArrowRight, Check, Cloud, GitBranch, GitMerge, Search, X } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import { mergeBlockedReason, mergeCandidates, mergeRef, type MergeRequest } from "../branches/mergeSelection";
  import { trapFocus } from "../ui/focusTrap";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { formatError } from "../ui/formatError";

  let { request, onClose }: { request: MergeRequest; onClose: () => void } = $props();
  let selectedRef = $state(untrack(() => request.sourceRef));
  let ffOnly = $state(untrack(() => request.ffOnly));
  let query = $state("");
  let scope = $state<"all" | "local" | "remote">("all");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let searchInput: HTMLInputElement | undefined = $state();
  let choicesEl: HTMLDivElement | undefined = $state();
  const MAX_VISIBLE = 100;
  let candidates = $derived(mergeCandidates($repoStore.branches, request.targetBranch));
  let localCount = $derived(candidates.filter(branch => !branch.is_remote).length);
  let remoteCount = $derived(candidates.length - localCount);
  let scopedCandidates = $derived(scope === "all" ? candidates : candidates.filter(branch => branch.is_remote === (scope === "remote")));
  let matches = $derived(query.trim() ? mergeCandidates(scopedCandidates, request.targetBranch, query) : scopedCandidates);
  let visible = $derived(matches.slice(0, MAX_VISIBLE));
  let source = $derived(candidates.find(branch => mergeRef(branch) === selectedRef));
  let selectionHidden = $derived(!!source && !visible.some(branch => mergeRef(branch) === selectedRef));
  let tabStop = $derived(visible.some(branch => mergeRef(branch) === selectedRef) ? selectedRef : visible[0] ? mergeRef(visible[0]) : "");
  let blocked = $derived(mergeBlockedReason($repoStore, request));
  let selectionMissing = $derived(selectedRef !== "" && !source);
  let conflicted = $derived($repoStore.operation.operation?.conflicted_total || $repoStore.statuses.filter(s => s.is_conflicted).length);

  // Child bindings settle after actions; focus only once the input is mounted.
  onMount(() => { searchInput?.focus(); });

  function close() {
    if (!busy) onClose();
  }

  function clearSelection() {
    selectedRef = "";
    error = null;
    searchInput?.focus();
  }

  async function showSelected() {
    if (!source || busy) return;
    // Exact matches rank first, including selections beyond the result cap.
    scope = source.is_remote ? "remote" : "local";
    query = source.name;
    await tick();
    const selected = choicesEl?.querySelector<HTMLButtonElement>('[aria-checked="true"]');
    selected?.focus();
    selected?.scrollIntoView({ block: "nearest" });
  }

  function navigateChoices(event: KeyboardEvent) {
    const buttons = [...(choicesEl?.querySelectorAll<HTMLButtonElement>("button[data-merge-choice]:not(:disabled)") ?? [])];
    if (!buttons.length) return;
    const index = buttons.findIndex(button => button === document.activeElement);
    const next = event.currentTarget === searchInput ? Math.max(0, buttons.findIndex(button => button.dataset.mergeRef === tabStop))
      : event.key === "ArrowDown" || event.key === "ArrowRight" ? (index + 1) % buttons.length
      : event.key === "ArrowUp" || event.key === "ArrowLeft" ? (index <= 0 ? buttons.length - 1 : index - 1)
      : event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : -1;
    if (next < 0) return;
    event.preventDefault();
    if (event.currentTarget instanceof HTMLButtonElement) {
      selectedRef = buttons[next].dataset.mergeRef ?? "";
      error = null;
    }
    buttons[next].focus();
  }

  async function submit() {
    // Read fresh state at the click boundary, including the captured destination.
    if (busy || mergeBlockedReason($repoStore, request)) return;
    const chosen = mergeCandidates($repoStore.branches, request.targetBranch).find(branch => mergeRef(branch) === selectedRef);
    if (!chosen) return;
    busy = true;
    error = null;
    try {
      const outcome = await repoStore.mergeBranch(mergeRef(chosen), ffOnly);
      if ($repoStore.currentPath !== request.repoPath) return;
      if (outcome.ok) {
        toastStore.success(`Merged ${chosen.name} into ${request.targetBranch}`);
        onClose();
      } else {
        error = outcome.error ?? "Merge failed. Review the repository and try again.";
        // Git may have started a conflicted merge even though the command failed.
        await repoStore.refresh(request.repoPath);
      }
    } catch (err) {
      error = formatError(err);
    } finally {
      busy = false;
    }
  }
</script>

<div
  use:portal={"body"}
  role="dialog"
  aria-modal="true"
  aria-labelledby="merge-branch-title"
  aria-describedby="merge-branch-description"
  aria-busy={busy}
  tabindex="-1"
  class="gp-scrim bg-black/40 flex items-center justify-center p-4"
  style="z-index: {LAYERS.MODAL}"
  onclick={(event) => event.target === event.currentTarget && close()}
  onkeydown={(event) => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(); } }}
>
  <div use:trapFocus={{ autofocus: false }} class="gp-card w-full max-w-lg max-h-[90vh] rounded-2xl shadow-float flex flex-col overflow-hidden text-textPrimary text-xs">
    <div class="p-5 pb-3 flex items-start justify-between gap-3">
      <div>
        <h2 id="merge-branch-title" class="flex items-center gap-2 text-base font-semibold"><GitMerge size={18} class="text-accent" /> Merge branches</h2>
        <p id="merge-branch-description" class="mt-1.5 text-textMuted">Choose a source branch to merge into your checked-out branch.</p>
      </div>
      <button type="button" class="gp-icon-btn shrink-0" aria-label="Close merge dialog" disabled={busy} onclick={close}><X size={16} /></button>
    </div>

    <div class="mx-5 mb-4 p-3 rounded-xl bg-background border border-border/60 grid grid-cols-[1fr_auto_1fr] items-center gap-3 max-h-36 overflow-y-auto shrink-0">
      <div class="min-w-0">
        <div class="flex items-center justify-between gap-1 mb-1">
          <p class="text-[10px] uppercase tracking-wider text-textMuted">From</p>
          {#if selectedRef}<button type="button" class="rounded p-0.5 text-textMuted hover:text-textPrimary" aria-label="Clear selected branch" title="Clear selected branch" disabled={busy} onclick={clearSelection}><X size={12} /></button>{/if}
        </div>
        <p class="font-mono break-all {source ? 'text-textPrimary' : 'text-textMuted'}">{source?.name ?? (selectionMissing ? "Branch unavailable" : "Choose a branch")}</p>
        {#if source}<p class="text-[10px] text-textMuted mt-1">{source.is_remote ? "Remote branch" : "Local branch"}</p>{/if}
      </div>
      <ArrowRight size={16} class="text-textMuted" />
      <div class="min-w-0"><p class="text-[10px] uppercase tracking-wider text-textMuted mb-1">Into current</p><p class="font-mono text-accent break-all">{request.targetBranch ?? "No branch checked out"}</p></div>
    </div>

    <div class="px-5 pb-3">
      <label for="merge-branch-search" class="sr-only">Search branches to merge</label>
      <div class="flex items-center gap-2 gp-field">
        <Search size={14} class="text-textMuted shrink-0" />
        <input id="merge-branch-search" bind:this={searchInput} bind:value={query} disabled={busy} placeholder="Search local and remote branches…" class="min-w-0 w-full bg-transparent outline-hidden" onkeydown={(event) => { if (event.key === "ArrowDown") navigateChoices(event); }} />
        {#if query}<button type="button" aria-label="Clear merge search" disabled={busy} onclick={() => { query = ""; searchInput?.focus(); }}><X size={13} /></button>{/if}
      </div>
      <div class="flex items-center gap-1.5 mt-3" role="group" aria-label="Branch origin">
        {#each [{ id: "all", label: "All", count: candidates.length }, { id: "local", label: "Local", count: localCount }, { id: "remote", label: "Remote", count: remoteCount }] as tab (tab.id)}
          <button type="button" aria-label="{tab.label} branches" aria-pressed={scope === tab.id} disabled={busy}
            class="rounded-full px-2.5 py-1 text-[11px] border transition-colors {scope === tab.id ? 'bg-accent/10 border-accent/40 text-accent' : 'border-border/60 text-textMuted hover:bg-surfaceHover'}"
            onclick={() => { if (tab.id === "all" || tab.id === "local" || tab.id === "remote") scope = tab.id; }}>
            {tab.label} <span class="opacity-70 ml-0.5">{tab.count}</span>
          </button>
        {/each}
      </div>
      <p class="mt-2 text-[10px] text-textMuted" role="status">{#if matches.length > MAX_VISIBLE}Showing {visible.length} of {matches.length} matches. Refine your search.{:else}{matches.length} {matches.length === 1 ? "branch" : "branches"} available{/if}</p>
      {#if selectionHidden}
        <div class="mt-2 flex items-center justify-between gap-2 text-[11px] text-textMuted" role="status">
          <span>Your selected source is outside these results.</span>
          <button type="button" class="shrink-0 text-accent hover:underline" disabled={busy} onclick={() => void showSelected()}>Show selected</button>
        </div>
      {/if}
    </div>

    <div bind:this={choicesEl} class="min-h-20 max-h-60 overflow-y-auto px-3 pb-2" role="radiogroup" aria-label="Source branches" aria-describedby="merge-choice-help">
      {#each visible as branch (mergeRef(branch))}
        {@const selected = selectedRef === mergeRef(branch)}
        <button type="button" role="radio" data-merge-choice data-merge-ref={mergeRef(branch)} aria-label="Select {branch.is_remote ? 'remote' : 'local'} branch {branch.name}" aria-checked={selected} tabindex={mergeRef(branch) === tabStop ? 0 : -1} disabled={busy}
          class="w-full flex items-center gap-2.5 text-left rounded-lg p-2.5 mb-0.5 focus-visible:outline-2 focus-visible:outline-accent {selected ? 'bg-accent/10 ring-1 ring-inset ring-accent/40' : 'hover:bg-surfaceHover'}"
          onclick={() => { selectedRef = mergeRef(branch); error = null; }} onkeydown={navigateChoices}>
          {#if branch.is_remote}<Cloud size={15} class="text-textMuted shrink-0" />{:else}<GitBranch size={15} class="text-textMuted shrink-0" />{/if}
          <span class="flex-1 min-w-0"><span class="block font-mono break-all">{branch.name}</span><span class="block text-[10px] text-textMuted truncate mt-0.5">{branch.is_remote ? "Remote" : "Local"}{branch.last_summary ? ` · ${branch.last_summary}` : ""}</span></span>
          {#if selected}<Check size={16} class="text-accent shrink-0" />{/if}
        </button>
      {:else}
        <div class="px-2 py-5 text-center text-textMuted">
          <p>{query.trim() ? "No branches match your search." : scope === "all" ? "No other branches are available to merge." : `No ${scope} branches are available to merge.`}</p>
          {#if query.trim() || scope !== "all"}<button type="button" class="text-accent mt-2 hover:underline" disabled={busy} onclick={() => { query = ""; scope = "all"; searchInput?.focus(); }}>Reset filters</button>{/if}
        </div>
      {/each}
    </div>

    <div class="p-5 pt-3 space-y-3 overflow-y-auto">
      <p id="merge-choice-help" class="text-[10px] text-textMuted">Use arrow keys to choose a branch. Tab moves to merge options.</p>
      {#if selectionMissing}<p role="alert" class="text-amber-600 dark:text-amber-400">The selected branch is no longer available. Choose another branch.</p>{/if}
      {#if blocked}<p role="status" class="flex items-start gap-2 text-amber-600 dark:text-amber-400"><AlertTriangle size={14} class="shrink-0 mt-0.5" />{blocked}</p>{/if}
      {#if error}<p role="alert" class="text-rose-500 break-words whitespace-pre-wrap">{error}</p>{/if}
      {#if $repoStore.statuses.length > 0 && !blocked && !error}<p class="text-textMuted">You have uncommitted changes. Git may require you to commit or stash them first.</p>{/if}
      <label class="flex items-start gap-2.5 cursor-pointer"><input type="checkbox" bind:checked={ffOnly} disabled={busy} class="accent-accent mt-0.5" /><span>Fast-forward only<span class="block text-[10px] text-textMuted mt-1">Only move the branch forward; stop if a merge commit is needed.</span></span></label>
      <div class="flex items-center justify-end gap-2 pt-1">
        {#if conflicted && $repoStore.currentPath === request.repoPath}<button type="button" class="gp-btn" disabled={busy} onclick={() => { repoStore.setActiveTab("work", "resolve"); close(); }}>Resolve conflicts</button>{/if}
        <button type="button" class="gp-btn" disabled={busy} onclick={close}>Cancel</button>
        <button type="button" class="gp-btn gp-btn-primary" disabled={busy || !!blocked || !source} onclick={() => void submit()}>{busy ? "Merging…" : "Merge branch"}</button>
      </div>
    </div>
  </div>
</div>
