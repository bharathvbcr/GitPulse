<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import {
    harnessStore,
    verdictDetail,
    verdictLabel,
    type AiGeneration,
    type PolicyVerdict,
  } from "../stores/harnessStore";
  import { Send, Sparkles, AlertTriangle, ShieldCheck, ShieldAlert, Loader } from "lucide-svelte";
  import { formatError } from "../ui/formatError";
  import { isImeComposition } from "../keyboard/imeGuard";

  let stagedFiles = $derived($repoStore.statuses.filter((s) => s.is_staged));
  let dirtyCount = $derived($repoStore.statuses.length);
  let conflictedCount = $derived($repoStore.statuses.filter((s) => s.is_conflicted).length);
  let aiReady = $derived($harnessStore.ai?.ready ?? false);
  let commitMessage = $derived($repoStore.commitDraft);
  let isAmending = $derived($repoStore.isAmending);
  let includeUnstaged = $state(false);
  let isGenerating = $state(false);
  let generation = $state<AiGeneration | null>(null);
  let aiError = $state<string | null>(null);
  let commitError = $state<string | null>(null);
  let lastVerdict = $state<PolicyVerdict | null>(null);

  let quickCommit = $derived(includeUnstaged && !isAmending);
  let commitCount = $derived(quickCommit ? dirtyCount : stagedFiles.length);
  let commitDisabled = $derived(
    !commitMessage.trim() ||
      conflictedCount > 0 ||
      (quickCommit ? dirtyCount === 0 : stagedFiles.length === 0 && !isAmending),
  );

  async function generateMessage() {
    const path = $repoStore.currentPath;
    if (!path || isGenerating) return;
    isGenerating = true;
    aiError = null;
    generation = null;
    try {
      const result = await harnessStore.generateCommitMessage(path);
      // The user may have switched repositories while generation ran; writing
      // the draft now would land it in the active session — the wrong repo.
      if ($repoStore.currentPath !== path) return;
      generation = result;
      repoStore.setCommitDraft(result.text);
    } catch (err: unknown) {
      aiError = formatError(err);
    } finally {
      isGenerating = false;
    }
  }

  async function finishCommit(outcome: { ok: boolean; error?: string; policy?: PolicyVerdict | null }) {
    lastVerdict = outcome.policy ?? null;
    if (!outcome.ok) {
      // A refused commit keeps its message: the user has to change the action,
      // not retype the description of it.
      commitError = outcome.error ?? "The commit did not run.";
      return;
    }
    repoStore.setCommitDraft("");
    repoStore.setAmending(false);
    generation = null;
  }

  async function handleCommit(forceQuick = false) {
    const message = commitMessage.trim();
    const useQuick = (forceQuick || includeUnstaged) && !isAmending;
    if (!message) return;
    if (conflictedCount > 0) return;
    if (useQuick ? dirtyCount === 0 : stagedFiles.length === 0 && !isAmending) return;
    commitError = null;
    if (useQuick) {
      await finishCommit(await repoStore.quickCommit(message));
      return;
    }
    await finishCommit(await repoStore.commit(message, isAmending));
  }

  function onMessageKeydown(event: KeyboardEvent) {
    if (isImeComposition(event)) return;
    if (!(event.metaKey || event.ctrlKey) || event.key !== "Enter") return;
    event.preventDefault();
    if (event.shiftKey && !isAmending) {
      includeUnstaged = true;
      void handleCommit(true);
      return;
    }
    void handleCommit();
  }

  /** "1.4s", "820ms" — a local model's latency is worth showing plainly. */
  function duration(ms: number): string {
    return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
  }
</script>

<div class="p-3 border-t border-border/60 bg-surface flex flex-col gap-2 shrink-0">
  <div class="flex items-center justify-between">
    <span class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Commit</span>
    <button
      onclick={generateMessage}
      disabled={isGenerating || stagedFiles.length === 0 || !aiReady}
      title={aiReady
        ? "Write a message for the staged diff with the local model"
        : ($harnessStore.ai?.detail ?? "No local model server is running")}
      class="gp-chip bg-accent/15 text-accent border-accent/40 hover:bg-accent/25 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
    >
      {#if isGenerating}
        <Loader size={12} class="animate-spin" />
        <span>Writing…</span>
      {:else}
        <Sparkles size={12} />
        <span>Generate</span>
      {/if}
    </button>
  </div>

  <textarea
    value={commitMessage}
    oninput={(e) => repoStore.setCommitDraft((e.currentTarget as HTMLTextAreaElement).value)}
    onkeydown={onMessageKeydown}
    placeholder="Commit message (e.g. feat: add auth)..."
    rows="3"
    class="w-full bg-background border border-border/80 rounded-xl p-2.5 text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-none focus:border-accent/60 resize-none font-mono transition-colors"
  ></textarea>

  {#if generation}
    <!-- Provenance for the suggestion: which model wrote it, against how much
         context, from how much of the diff, and how long it took. -->
    <div class="text-[10px] text-textMuted leading-relaxed">
      <span class="font-mono">{generation.model}</span>
      · {generation.context_source}
      {#if generation.prompt_tokens > 0}
        · {generation.prompt_tokens} prompt tokens
      {/if}
      · {duration(generation.elapsed_ms)}
      {#if !generation.budget.planned_by_harness}
        · <span class="text-amber-400">budget estimated locally</span>
      {/if}
    </div>
    {#each generation.warnings as warning}
      <div class="text-[10px] text-amber-400 flex items-start gap-1">
        <AlertTriangle size={11} class="mt-px shrink-0" />
        <span>{warning}</span>
      </div>
    {/each}
  {/if}

  {#if aiError}
    <div class="text-[10px] text-rose-400 flex items-start gap-1">
      <AlertTriangle size={11} class="mt-px shrink-0" />
      <span>{aiError}</span>
    </div>
  {/if}

  {#if commitError}
    <div class="text-[10px] text-rose-400 whitespace-pre-wrap font-mono leading-relaxed">{commitError}</div>
  {:else if lastVerdict}
    <div
      class="text-[10px] flex items-start gap-1 {lastVerdict.status === 'unchecked'
        ? 'text-amber-400'
        : 'text-textMuted'}"
      title={verdictDetail(lastVerdict)}
    >
      {#if lastVerdict.checked}
        <ShieldCheck size={11} class="mt-px shrink-0" />
      {:else}
        <ShieldAlert size={11} class="mt-px shrink-0" />
      {/if}
      <span>{verdictLabel(lastVerdict)}</span>
    </div>
  {/if}

  <div class="flex items-center justify-between gap-2">
    <div class="flex items-center gap-3 min-w-0">
      <label class="flex items-center gap-1.5 text-[11px] text-textMuted cursor-pointer">
        <input
          type="checkbox"
          checked={isAmending}
          onchange={(e) => repoStore.setAmending((e.currentTarget as HTMLInputElement).checked)}
          class="rounded accent-accent"
        />
        <span>Amend</span>
      </label>
      <label
        class="flex items-center gap-1.5 text-[11px] text-textMuted cursor-pointer {isAmending
          ? 'opacity-40 cursor-not-allowed'
          : ''}"
        title="Stage remaining files and commit them together (quick commit)"
      >
        <input
          type="checkbox"
          checked={includeUnstaged}
          disabled={isAmending}
          aria-label="Include unstaged files in this commit"
          onchange={(e) => (includeUnstaged = (e.currentTarget as HTMLInputElement).checked)}
          class="rounded accent-accent"
        />
        <span>Include unstaged</span>
      </label>
    </div>
    <button
      onclick={() => void handleCommit()}
      disabled={commitDisabled}
      title={quickCommit
        ? "Stage all changes and commit (Cmd/Ctrl+Shift+Enter)"
        : "Commit staged files (Cmd/Ctrl+Enter)"}
      class="gp-btn-primary"
    >
      <Send size={12} />
      <span>{quickCommit ? "Commit all" : "Commit"} ({commitCount})</span>
    </button>
  </div>
</div>
